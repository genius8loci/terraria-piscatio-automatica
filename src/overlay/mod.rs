//! Оверлей поверх игры: перехват Direct3D9 и свой мини-рендер.
//!
//! Три вещи, которые здесь сделаны не «как принято», а по замерам на живой игре:
//!
//! 1. **Патчим функцию, а не vtable.** vtable девайса лежит в куче
//!    (`0x1D166C9C`), а не в образе `d3d9.dll` (`0x68840000-0x689BA000`):
//!    таблица своя на каждый девайс, поэтому подмена слота у пробного девайса
//!    игру не задевает. Inline-детур на саму реализацию — задевает.
//!
//! 2. **Цепляемся к `Present`, а не к `EndScene`.** Terraria рисует в несколько
//!    проходов через render target'ы, и `EndScene` вызывается по нескольку раз
//!    за кадр. Рисуя там, мы попадали в промежуточные цели, а игра потом
//!    использовала их как текстуры — панель размножалась по деревьям и воде.
//!    `Present` вызывается ровно один раз за кадр и уже по финальной цели.
//!
//! 3. **Ресурсы храним сырыми указателями, без `Drop`.** Иначе TLS-деструктор
//!    зарегистрируется в нашем модуле, а после выгрузки DLL сработает по уже
//!    отображённой памяти и уронит игру при выходе.

mod font;
mod icons;
pub mod state;
mod ui;
mod xnb;

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use minhook::MinHook;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D9::{
    D3DBLEND_INVSRCALPHA, D3DBLEND_SRCALPHA, D3DCREATE_SOFTWARE_VERTEXPROCESSING, D3DCULL_NONE,
    D3DDEVTYPE_HAL, D3DFMT_A8R8G8B8, D3DFMT_UNKNOWN, D3DFVF_DIFFUSE, D3DFVF_TEX1, D3DFVF_XYZRHW,
    D3DLOCKED_RECT, D3DPOOL_DEFAULT, D3DPRESENT_PARAMETERS, D3DPT_TRIANGLELIST,
    D3DRS_ALPHABLENDENABLE, D3DRS_CULLMODE, D3DRS_DESTBLEND, D3DRS_FOGENABLE, D3DRS_LIGHTING,
    D3DRS_SCISSORTESTENABLE, D3DRS_SRCBLEND, D3DRS_STENCILENABLE, D3DRS_ZENABLE, D3DSBT_ALL,
    D3DSWAPEFFECT_DISCARD, D3DTA_DIFFUSE, D3DTA_TEXTURE, D3DTOP_MODULATE, D3DTOP_SELECTARG1,
    D3DTSS_ALPHAARG1, D3DTSS_ALPHAARG2, D3DTSS_ALPHAOP, D3DTSS_COLORARG1, D3DTSS_COLORARG2,
    D3DTSS_COLOROP, D3DUSAGE_DYNAMIC, D3DVIEWPORT9, Direct3DCreate9, Direct3DCreate9Ex, IDirect3D9,
    IDirect3D9Ex, IDirect3DDevice9, IDirect3DDevice9Ex, IDirect3DStateBlock9, IDirect3DTexture9,
};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;
use windows::core::{HRESULT, Interface};

use crate::SHOW_UI;
use icons::IconAtlas;
use xnb::GameFont;

/// Палитра под инвентарные панели Terraria.
pub mod colors {
    pub const BORDER: u32 = 0xFF_18183A;
    pub const FRAME: u32 = 0xFF_5A5CB8;
    pub const BACK: u32 = 0xF2_2E3070;
    pub const ROW: u32 = 0xFF_3A3D82;
    pub const ROW_BORDER: u32 = 0xFF_252858;
    pub const TITLE: u32 = 0xFF_FFD75E;
    pub const TEXT: u32 = 0xFF_E4E4F2;
    pub const MUTED: u32 = 0xFF_9FA3D8;
    pub const VALUE: u32 = 0xFF_FFC94A;
    pub const ON: u32 = 0xFF_3FA34D;
    pub const ON_TEXT: u32 = 0xFF_EAFBEA;
    pub const OFF: u32 = 0xFF_8A3A3A;
    pub const OFF_TEXT: u32 = 0xFF_F5E2E2;
    pub const KNOB: u32 = 0xFF_1E1F42;
    pub const TAB: u32 = 0xFF_3A3D82;
    pub const TAB_HOVER: u32 = 0xFF_474BA0;
    pub const TAB_ACTIVE: u32 = 0xFF_5A5CB8;
    pub const SLOT: u32 = 0xFF_2A2C60;
    pub const SLOT_ON: u32 = 0xFF_2F5B38;
    pub const SLOT_ON_BORDER: u32 = 0xFF_5CD16E;
    pub const MARK_ALLOW: u32 = 0xFF_5CD16E;
    pub const MARK_DENY: u32 = 0xFF_D95757;
}

const D3D_SDK_VERSION: u32 = 32;
const SLOT_RESET: usize = 16;
const SLOT_PRESENT: usize = 17;

const FVF: u32 = D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1;

type FnPresent = unsafe extern "system" fn(
    *mut c_void,
    *const RECT,
    *const RECT,
    *mut c_void,
    *const c_void,
) -> HRESULT;
type FnReset = unsafe extern "system" fn(*mut c_void, *mut D3DPRESENT_PARAMETERS) -> HRESULT;

/// Реализаций может быть две — для обычного девайса и для Ex.
const MAX_HOOKS: usize = 2;
static TARGET_PRESENT: [AtomicUsize; MAX_HOOKS] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static TARGET_RESET: [AtomicUsize; MAX_HOOKS] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static TRAMPOLINE_PRESENT: [AtomicUsize; MAX_HOOKS] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static TRAMPOLINE_RESET: [AtomicUsize; MAX_HOOKS] = [AtomicUsize::new(0), AtomicUsize::new(0)];

static INSTALLED: AtomicBool = AtomicBool::new(false);
/// Пока false, перехватчики только зовут оригинал: нужно при выгрузке.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static FIRST_FRAME_LOGGED: AtomicBool = AtomicBool::new(false);
/// Просьба освободить ресурсы; выполняется на потоке рендера.
static RELEASE_REQUESTED: AtomicBool = AtomicBool::new(false);
static RELEASED: AtomicBool = AtomicBool::new(false);

/// Ресурсы держим сырыми указателями: у них не должно быть `Drop`,
/// иначе после выгрузки DLL деструктор сработает по чужой памяти.
static FONT_TEXTURE: AtomicUsize = AtomicUsize::new(0);
static ICON_TEXTURE: AtomicUsize = AtomicUsize::new(0);
static STATE_BLOCK: AtomicUsize = AtomicUsize::new(0);

/// Пары «подпись — значение»: шрифт игры пропорциональный,
/// поэтому колонки выравниваем координатами, а не пробелами.
static FONT: OnceLock<Option<GameFont>> = OnceLock::new();
static ICONS: Mutex<Option<IconAtlas>> = Mutex::new(None);
/// Набор предметов, под который собран текущий атлас.
static ICON_ITEMS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    x: f32,
    y: f32,
    z: f32,
    rhw: f32,
    color: u32,
    u: f32,
    v: f32,
}

// ---------------------------------------------------------------------------
// Установка и снятие
// ---------------------------------------------------------------------------

pub fn install() -> bool {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return true;
    }

    let mut presents: Vec<usize> = Vec::new();
    let mut resets: Vec<usize> = Vec::new();
    for (present, reset) in [dummy_targets_ex(), dummy_targets()].into_iter().flatten() {
        if present != 0 && !presents.contains(&present) {
            presents.push(present);
        }
        if reset != 0 && !resets.contains(&reset) {
            resets.push(reset);
        }
    }

    if presents.is_empty() {
        crate::log!("оверлей: не удалось получить адрес Present");
        INSTALLED.store(false, Ordering::SeqCst);
        return false;
    }

    let present_hooks: [usize; MAX_HOOKS] = [
        hook_present_0 as FnPresent as usize,
        hook_present_1 as FnPresent as usize,
    ];
    let reset_hooks: [usize; MAX_HOOKS] = [
        hook_reset_0 as FnReset as usize,
        hook_reset_1 as FnReset as usize,
    ];

    let mut installed = 0usize;
    for (i, target) in presents.iter().take(MAX_HOOKS).enumerate() {
        crate::log!(
            "оверлей: Present по 0x{target:08X} в {}",
            module_of(*target)
        );
        if let Some(trampoline) = hook(*target, present_hooks[i]) {
            TARGET_PRESENT[i].store(*target, Ordering::SeqCst);
            TRAMPOLINE_PRESENT[i].store(trampoline, Ordering::SeqCst);
            installed += 1;
        } else {
            crate::log!("оверлей: детур Present 0x{target:08X} не встал");
        }
    }
    for (i, target) in resets.iter().take(MAX_HOOKS).enumerate() {
        if let Some(trampoline) = hook(*target, reset_hooks[i]) {
            TARGET_RESET[i].store(*target, Ordering::SeqCst);
            TRAMPOLINE_RESET[i].store(trampoline, Ordering::SeqCst);
        }
    }

    if installed == 0 {
        crate::log!("оверлей: ни один детур не встал");
        INSTALLED.store(false, Ordering::SeqCst);
        return false;
    }

    if FONT.get_or_init(load_font).is_none() {
        crate::log!("оверлей: шрифт загрузить не вышло, текста не будет");
    }
    ACTIVE.store(true, Ordering::SeqCst);
    true
}

/// Снимает детуры. Обязательно вызывать перед выгрузкой DLL: иначе следующий
/// кадр прыгнет по адресу уже отображённого кода и уронит игру.
pub fn uninstall() {
    if !INSTALLED.swap(false, Ordering::SeqCst) {
        return;
    }

    // Порядок здесь важен. Сначала гасим отрисовку: иначе кадр между
    // освобождением и снятием хуков успевает создать ресурсы заново,
    // и новая пара остаётся висеть.
    ACTIVE.store(false, Ordering::SeqCst);

    // Ресурсы в D3DPOOL_DEFAULT нельзя просто бросить: пока они живы,
    // устройство не может сделать Reset. Игра после этого падает при
    // сворачивании — XNA освобождает свои render target'ы, а пересоздать
    // их не может. Поэтому просим поток рендера освободить наши ресурсы
    // и ждём подтверждения, и только потом снимаем хуки.
    if FONT_TEXTURE.load(Ordering::SeqCst) != 0
        || STATE_BLOCK.load(Ordering::SeqCst) != 0
        || ICON_TEXTURE.load(Ordering::SeqCst) != 0
    {
        RELEASED.store(false, Ordering::SeqCst);
        RELEASE_REQUESTED.store(true, Ordering::SeqCst);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(600);
        while !RELEASED.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if !RELEASED.load(Ordering::SeqCst) {
            // Кадры не идут (игра свёрнута или встала) — освобождаем сами.
            // Это хуже, чем с потока рендера, но утечка гарантированно
            // ломает Reset, а так есть шанс обойтись.
            release_resources("поток рендера не ответил");
        }
    }

    for i in 0..MAX_HOOKS {
        for slot in [&TARGET_PRESENT[i], &TARGET_RESET[i]] {
            let target = slot.swap(0, Ordering::SeqCst);
            if target == 0 {
                continue;
            }
            unsafe {
                let _ = MinHook::disable_hook(target as *mut c_void);
                let _ = MinHook::remove_hook(target as *mut c_void);
            }
        }
    }
    crate::log!("оверлей: детуры сняты");
}

/// Родной шрифт игры, а если его нет — системный через GDI.
fn load_font() -> Option<GameFont> {
    if let Some(path) = game_font_path() {
        match xnb::load(&path, &font::charset()) {
            Some(loaded) => {
                crate::log!(
                    "оверлей: шрифт игры загружен ({}x{}, глифов {})",
                    loaded.width,
                    loaded.height,
                    loaded.glyphs.len()
                );
                return Some(loaded);
            }
            None => crate::log!(
                "оверлей: {} разобрать не вышло, беру системный",
                path.display()
            ),
        }
    } else {
        crate::log!("оверлей: папка игры не найдена, беру системный шрифт");
    }
    font::build()
}

/// Каталог с картинками игры: `Content/Images` рядом с exe.
pub fn content_dir() -> Option<std::path::PathBuf> {
    let dir = game_dir()?.join("Content").join("Images");
    dir.is_dir().then_some(dir)
}

/// Папка, где лежит исполняемый файл игры.
fn game_dir() -> Option<std::path::PathBuf> {
    let mut buffer = [0u16; 260];
    let len = unsafe { GetModuleFileNameW(None, &mut buffer) } as usize;
    if len == 0 || len >= buffer.len() {
        return None;
    }
    let exe = std::path::PathBuf::from(String::from_utf16_lossy(&buffer[..len]));
    exe.parent().map(|p| p.to_path_buf())
}

/// `Content/Fonts/Mouse_Text.xnb` рядом с исполняемым файлом игры.
fn game_font_path() -> Option<std::path::PathBuf> {
    let path = game_dir()?
        .join("Content")
        .join("Fonts")
        .join("Mouse_Text.xnb");
    path.exists().then_some(path)
}

/// Ставит inline-детур и возвращает адрес трамплина (оригинала).
fn hook(target: usize, detour: usize) -> Option<usize> {
    unsafe {
        let original = MinHook::create_hook(target as *mut c_void, detour as *mut c_void).ok()?;
        MinHook::enable_hook(target as *mut c_void).ok()?;
        Some(original as usize)
    }
}

/// Имя модуля, которому принадлежит адрес — для диагностики в логе.
fn module_of(address: usize) -> String {
    unsafe {
        let mut module = Default::default();
        if GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            windows::core::PCWSTR(address as *const u16),
            &mut module,
        )
        .is_err()
        {
            return "вне модулей (куча)".to_string();
        }
        let mut buffer = [0u16; 260];
        let len = GetModuleFileNameW(Some(module), &mut buffer) as usize;
        if len == 0 {
            return "неизвестный модуль".to_string();
        }
        let path = String::from_utf16_lossy(&buffer[..len]);
        path.rsplit('\\').next().unwrap_or(&path).to_string()
    }
}

fn present_parameters() -> D3DPRESENT_PARAMETERS {
    D3DPRESENT_PARAMETERS {
        Windowed: true.into(),
        SwapEffect: D3DSWAPEFFECT_DISCARD,
        BackBufferFormat: D3DFMT_UNKNOWN,
        hDeviceWindow: unsafe { GetDesktopWindow() },
        ..Default::default()
    }
}

/// Адреса `Present` и `Reset` из vtable живого девайса.
///
/// Читать таблицу обязательно **до** освобождения девайса: она лежит в куче
/// и живёт ровно столько, сколько сам девайс.
unsafe fn targets_of(device: *mut c_void) -> (usize, usize) {
    unsafe {
        let vtable = *(device as *const *const usize);
        (*vtable.add(SLOT_PRESENT), *vtable.add(SLOT_RESET))
    }
}

fn dummy_targets() -> Option<(usize, usize)> {
    unsafe {
        let d3d: IDirect3D9 = Direct3DCreate9(D3D_SDK_VERSION)?;
        let mut parameters = present_parameters();
        let mut device: Option<IDirect3DDevice9> = None;
        d3d.CreateDevice(
            0,
            D3DDEVTYPE_HAL,
            GetDesktopWindow(),
            D3DCREATE_SOFTWARE_VERTEXPROCESSING as u32,
            &mut parameters,
            &mut device,
        )
        .ok()?;
        let device = device?;
        Some(targets_of(device.as_raw()))
    }
}

fn dummy_targets_ex() -> Option<(usize, usize)> {
    unsafe {
        let d3d: IDirect3D9Ex = Direct3DCreate9Ex(D3D_SDK_VERSION).ok()?;
        let mut parameters = present_parameters();
        let mut device = None;
        d3d.CreateDeviceEx(
            0,
            D3DDEVTYPE_HAL,
            GetDesktopWindow(),
            D3DCREATE_SOFTWARE_VERTEXPROCESSING as u32,
            &mut parameters,
            std::ptr::null_mut(),
            &mut device,
        )
        .ok()?;
        let device: IDirect3DDevice9Ex = device?;
        Some(targets_of(device.as_raw()))
    }
}

// ---------------------------------------------------------------------------
// Перехватчики
// ---------------------------------------------------------------------------

unsafe extern "system" fn hook_present_0(
    device: *mut c_void,
    source: *const RECT,
    dest: *const RECT,
    window: *mut c_void,
    dirty: *const c_void,
) -> HRESULT {
    unsafe { present(0, device, source, dest, window, dirty) }
}

unsafe extern "system" fn hook_present_1(
    device: *mut c_void,
    source: *const RECT,
    dest: *const RECT,
    window: *mut c_void,
    dirty: *const c_void,
) -> HRESULT {
    unsafe { present(1, device, source, dest, window, dirty) }
}

unsafe extern "system" fn hook_reset_0(
    device: *mut c_void,
    parameters: *mut D3DPRESENT_PARAMETERS,
) -> HRESULT {
    unsafe { reset(0, device, parameters) }
}

unsafe extern "system" fn hook_reset_1(
    device: *mut c_void,
    parameters: *mut D3DPRESENT_PARAMETERS,
) -> HRESULT {
    unsafe { reset(1, device, parameters) }
}

unsafe fn present(
    slot: usize,
    device: *mut c_void,
    source: *const RECT,
    dest: *const RECT,
    window: *mut c_void,
    dirty: *const c_void,
) -> HRESULT {
    if RELEASE_REQUESTED.swap(false, Ordering::SeqCst) {
        release_resources("по запросу выгрузки");
        RELEASED.store(true, Ordering::SeqCst);
    }

    if ACTIVE.load(Ordering::Relaxed) {
        crate::FRAME.fetch_add(1, Ordering::Relaxed);
        if !FIRST_FRAME_LOGGED.swap(true, Ordering::SeqCst) {
            crate::log!("оверлей: первый кадр перехвачен, рендер работает");
        }
        if SHOW_UI.load(Ordering::Relaxed) {
            // Паника внутри чужого кадра убьёт игру — гасим на месте.
            let _ = catch_unwind(AssertUnwindSafe(|| unsafe { draw(device) }));
        }
    }

    let original = TRAMPOLINE_PRESENT[slot].load(Ordering::Relaxed);
    if original == 0 {
        return HRESULT(0);
    }
    unsafe {
        let call: FnPresent = std::mem::transmute(original);
        call(device, source, dest, window, dirty)
    }
}

unsafe fn reset(
    slot: usize,
    device: *mut c_void,
    parameters: *mut D3DPRESENT_PARAMETERS,
) -> HRESULT {
    // Ресурсы в D3DPOOL_DEFAULT обязаны быть освобождены до Reset.
    release_resources("хук Reset");

    let original = TRAMPOLINE_RESET[slot].load(Ordering::Relaxed);
    if original == 0 {
        return HRESULT(0);
    }
    unsafe {
        let call: FnReset = std::mem::transmute(original);
        call(device, parameters)
    }
}

// ---------------------------------------------------------------------------
// Ресурсы
// ---------------------------------------------------------------------------

fn release_resources(reason: &str) {
    let texture = FONT_TEXTURE.swap(0, Ordering::SeqCst);
    if texture != 0 {
        unsafe { drop(IDirect3DTexture9::from_raw(texture as *mut c_void)) };
    }
    let icons = ICON_TEXTURE.swap(0, Ordering::SeqCst);
    if icons != 0 {
        unsafe { drop(IDirect3DTexture9::from_raw(icons as *mut c_void)) };
        ICONS_STALE.store(true, Ordering::SeqCst);
    }
    let block = STATE_BLOCK.swap(0, Ordering::SeqCst);
    if block != 0 {
        unsafe { drop(IDirect3DStateBlock9::from_raw(block as *mut c_void)) };
    }
    if texture != 0 || block != 0 || icons != 0 {
        crate::log!("оверлей: ресурсы освобождены ({reason})");
    }
}

fn ensure_resources(device: &IDirect3DDevice9) -> bool {
    if FONT_TEXTURE.load(Ordering::Relaxed) != 0 && STATE_BLOCK.load(Ordering::Relaxed) != 0 {
        return true;
    }
    // Если уцелела половина пары, освобождаем её: иначе она утечёт
    // и не даст устройству сделать Reset.
    release_resources("пересоздание");

    let Some(Some(atlas)) = FONT.get() else {
        return false;
    };
    let Some(texture) = create_font_texture(device, atlas) else {
        crate::log!("оверлей: не удалось создать текстуру шрифта");
        return false;
    };
    let block = match unsafe { device.CreateStateBlock(D3DSBT_ALL) } {
        Ok(b) => b,
        Err(e) => {
            crate::log!("оверлей: CreateStateBlock не удался: {e}");
            return false;
        }
    };
    let texture_raw = texture.into_raw() as usize;
    let block_raw = block.into_raw() as usize;
    FONT_TEXTURE.store(texture_raw, Ordering::SeqCst);
    STATE_BLOCK.store(block_raw, Ordering::SeqCst);
    crate::log!("оверлей: ресурсы созданы (текстура 0x{texture_raw:08X}, блок 0x{block_raw:08X})");
    true
}

fn create_font_texture(device: &IDirect3DDevice9, atlas: &GameFont) -> Option<IDirect3DTexture9> {
    create_texture(device, atlas.width, atlas.height, &atlas.pixels)
}

/// Текстура A8R8G8B8 в D3DPOOL_DEFAULT, заполненная готовыми пикселями.
fn create_texture(
    device: &IDirect3DDevice9,
    width: u32,
    height: u32,
    pixels: &[u32],
) -> Option<IDirect3DTexture9> {
    unsafe {
        let mut texture: Option<IDirect3DTexture9> = None;
        device
            .CreateTexture(
                width,
                height,
                1,
                D3DUSAGE_DYNAMIC as u32,
                D3DFMT_A8R8G8B8,
                D3DPOOL_DEFAULT,
                &mut texture,
                std::ptr::null_mut(),
            )
            .ok()?;
        let texture = texture?;

        let mut locked = D3DLOCKED_RECT::default();
        texture
            .LockRect(0, &mut locked, std::ptr::null::<RECT>(), 0)
            .ok()?;
        for row in 0..height as usize {
            let destination =
                (locked.pBits as *mut u8).add(row * locked.Pitch as usize) as *mut u32;
            let source = pixels.as_ptr().add(row * width as usize);
            std::ptr::copy_nonoverlapping(source, destination, width as usize);
        }
        texture.UnlockRect(0).ok()?;
        Some(texture)
    }
}

// ---------------------------------------------------------------------------
// Художник
// ---------------------------------------------------------------------------

/// Накопитель геометрии кадра: три партии, по одной на текстуру.
pub struct Painter<'a> {
    solid: &'a mut Vec<Vertex>,
    text: &'a mut Vec<Vertex>,
    icons: &'a mut Vec<Vertex>,
    font: Option<&'a GameFont>,
    atlas: Option<&'a IconAtlas>,
    pub scale: f32,
}

impl<'a> Painter<'a> {
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: u32) {
        push_quad(self.solid, x, y, w, h, color, 0.0, 0.0, 0.0, 0.0);
    }

    /// Треугольник рисуем горизонтальными полосками: в шрифте игры
    /// стрелок нет, а квады у нас всё равно единственный примитив.
    pub fn triangle(&mut self, cx: f32, cy: f32, size: f32, up: bool, color: u32) {
        let steps = size.max(2.0) as i32;
        let step_h = size / steps as f32 + 1.0;
        for i in 0..steps {
            let t = i as f32 / steps as f32;
            let half = size * (1.0 - t);
            let dy = if up {
                -size * 0.5 + t * size
            } else {
                size * 0.5 - t * size
            };
            self.rect(cx - half, cy + dy, half * 2.0, step_h, color);
        }
    }

    pub fn text(&mut self, x: f32, y: f32, value: &str, color: u32) {
        let Some(font) = self.font else {
            return;
        };
        let scale = self.scale;
        let tw = font.width as f32;
        let th = font.height as f32;
        let mut pen = x;
        for ch in value.chars() {
            let Some(glyph) = font.glyphs.get(&ch) else {
                pen += font.space_advance * scale;
                continue;
            };
            if glyph.w > 0 && glyph.h > 0 {
                push_quad(
                    self.text,
                    (pen + glyph.off_x * scale).round(),
                    (y + glyph.off_y * scale).round(),
                    glyph.w as f32 * scale,
                    glyph.h as f32 * scale,
                    color,
                    glyph.sx as f32 / tw,
                    glyph.sy as f32 / th,
                    (glyph.sx + glyph.w) as f32 / tw,
                    (glyph.sy + glyph.h) as f32 / th,
                );
            }
            pen += glyph.advance * scale;
        }
    }

    pub fn measure(&self, value: &str) -> f32 {
        let Some(font) = self.font else {
            return 0.0;
        };
        value
            .chars()
            .map(|ch| {
                font.glyphs
                    .get(&ch)
                    .map(|g| g.advance)
                    .unwrap_or(font.space_advance)
            })
            .sum::<f32>()
            * self.scale
    }

    pub fn text_centered(&mut self, x: f32, y: f32, w: f32, h: f32, value: &str, color: u32) {
        let width = self.measure(value);
        let line = self.font.map(|f| f.line_height).unwrap_or(20.0) * self.scale;
        self.text(x + (w - width) * 0.5, y + (h - line) * 0.5, value, color);
    }

    pub fn text_right(&mut self, x: f32, y: f32, w: f32, value: &str, color: u32) {
        let width = self.measure(value);
        self.text(x + w - width, y, value, color);
    }

    /// Иконка предмета вписывается в клетку с сохранением пропорций.
    pub fn icon(&mut self, item: i32, x: f32, y: f32, w: f32, h: f32) {
        let Some(atlas) = self.atlas else {
            return;
        };
        let Some(rect) = atlas.get(item) else {
            return;
        };
        let pad = 8.0 * self.scale;
        let fit = ((w - pad) / rect.w as f32).min((h - pad) / rect.h as f32);
        let dw = rect.w as f32 * fit;
        let dh = rect.h as f32 * fit;
        let tw = atlas.width as f32;
        let th = atlas.height as f32;
        push_quad(
            self.icons,
            (x + (w - dw) * 0.5).round(),
            (y + (h - dh) * 0.5).round(),
            dw,
            dh,
            0xFFFF_FFFF,
            rect.x as f32 / tw,
            rect.y as f32 / th,
            (rect.x + rect.w) as f32 / tw,
            (rect.y + rect.h) as f32 / th,
        );
    }
}

// ---------------------------------------------------------------------------
// Рендер
// ---------------------------------------------------------------------------

/// Состояние интерфейса живёт на потоке рендера.
static UI: Mutex<Option<ui::UiState>> = Mutex::new(None);
/// Прошлое состояние кнопки — чтобы ловить нажатие по фронту.
static MOUSE_WAS_DOWN: AtomicBool = AtomicBool::new(false);
/// Атлас иконок устарел и требует пересборки.
static ICONS_STALE: AtomicBool = AtomicBool::new(false);

/// Просит пересобрать атлас иконок под новый набор предметов.
pub fn set_icon_items(items: Vec<i32>) {
    if let Ok(mut slot) = ICON_ITEMS.lock() {
        if *slot == items {
            return;
        }
        *slot = items;
    }
    ICONS_STALE.store(true, Ordering::SeqCst);
}

/// Собирает атлас иконок, если его ещё нет или набор поменялся.
fn ensure_icons(device: &IDirect3DDevice9) {
    if !ICONS_STALE.swap(false, Ordering::SeqCst) && ICON_TEXTURE.load(Ordering::Relaxed) != 0 {
        return;
    }
    let items = ICON_ITEMS.lock().map(|i| i.clone()).unwrap_or_default();
    if items.is_empty() {
        return;
    }
    let Some(content) = crate::overlay::content_dir() else {
        return;
    };
    let Some(atlas) = icons::build(&content, &items) else {
        crate::log!("оверлей: атлас иконок собрать не вышло");
        return;
    };
    crate::log!(
        "оверлей: атлас иконок {}x{}, предметов {}",
        atlas.width,
        atlas.height,
        atlas.len()
    );

    let previous = ICON_TEXTURE.swap(0, Ordering::SeqCst);
    if previous != 0 {
        unsafe { drop(IDirect3DTexture9::from_raw(previous as *mut c_void)) };
    }
    if let Some(texture) = create_texture(device, atlas.width, atlas.height, &atlas.pixels) {
        ICON_TEXTURE.store(texture.into_raw() as usize, Ordering::SeqCst);
    }
    if let Ok(mut slot) = ICONS.lock() {
        *slot = Some(atlas);
    }
}

unsafe fn draw(raw: *mut c_void) {
    let Some(device) = (unsafe { IDirect3DDevice9::from_raw_borrowed(&raw) }) else {
        return;
    };
    if !ensure_resources(device) {
        return;
    }
    ensure_icons(device);

    let mut solid: Vec<Vertex> = Vec::with_capacity(512);
    let mut text: Vec<Vertex> = Vec::with_capacity(2048);
    let mut icon_quads: Vec<Vertex> = Vec::with_capacity(1024);

    let screen = unsafe {
        let mut viewport = D3DVIEWPORT9::default();
        match device.GetViewport(&mut viewport) {
            Ok(()) => (viewport.Width as f32, viewport.Height as f32),
            Err(_) => (1280.0, 720.0),
        }
    };

    // Ввод берём глазами игры: те же координаты, что и у нашей отрисовки.
    let (mx, my, down) = crate::input::cursor().unwrap_or((-1, -1, false));
    let clicked = down && !MOUSE_WAS_DOWN.load(Ordering::Relaxed);
    MOUSE_WAS_DOWN.store(down, Ordering::Relaxed);
    let input = ui::Input {
        x: mx as f32,
        y: my as f32,
        clicked,
    };

    let over_ui = {
        let font = FONT.get().and_then(|f| f.as_ref());
        let atlas_guard = ICONS.lock().ok();
        let atlas = atlas_guard.as_ref().and_then(|g| g.as_ref());
        let mut painter = Painter {
            solid: &mut solid,
            text: &mut text,
            icons: &mut icon_quads,
            font,
            atlas,
            scale: 1.0,
        };
        let Ok(mut guard) = UI.lock() else {
            return;
        };
        let ui_state = guard.get_or_insert_with(ui::UiState::default);
        ui::build(&mut painter, ui_state, input, screen)
    };

    // Пока курсор над окном, игра не должна считать клик игровым.
    crate::input::set_mouse_interface(over_ui);

    let texture_ptr = FONT_TEXTURE.load(Ordering::Relaxed);
    let block_ptr = STATE_BLOCK.load(Ordering::Relaxed);
    let icon_ptr = ICON_TEXTURE.load(Ordering::Relaxed);
    if texture_ptr == 0 || block_ptr == 0 {
        return;
    }
    let texture_raw = texture_ptr as *mut c_void;
    let block_raw = block_ptr as *mut c_void;
    let icon_raw = icon_ptr as *mut c_void;
    let (Some(font_texture), Some(block)) = (unsafe {
        (
            IDirect3DTexture9::from_raw_borrowed(&texture_raw),
            IDirect3DStateBlock9::from_raw_borrowed(&block_raw),
        )
    }) else {
        return;
    };
    let icon_texture = if icon_ptr == 0 {
        None
    } else {
        unsafe { IDirect3DTexture9::from_raw_borrowed(&icon_raw) }
    };

    unsafe {
        // В Present сцена уже закрыта, а DrawPrimitiveUP работает только
        // внутри сцены — открываем свою.
        if device.BeginScene().is_err() {
            return;
        }
        let _ = block.Capture();
        apply_states(device);

        if !solid.is_empty() {
            let _ = device.SetTexture(0, None);
            let _ = device.SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1.0 as u32);
            let _ = device.SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
            let _ = device.SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1.0 as u32);
            let _ = device.SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_DIFFUSE);
            draw_batch(device, &solid);
        }

        if !icon_quads.is_empty() {
            if let Some(icons) = icon_texture {
                let _ = device.SetTexture(0, icons);
                modulate_stage(device);
                draw_batch(device, &icon_quads);
            }
        }

        if !text.is_empty() {
            let _ = device.SetTexture(0, font_texture);
            modulate_stage(device);
            draw_batch(device, &text);
        }

        // Текстуру снимаем явно: если она останется в стейдже, игра
        // нарисует ей свои спрайты.
        let _ = device.SetTexture(0, None);
        let _ = block.Apply();
        let _ = device.EndScene();
    }
}

unsafe fn draw_batch(device: &IDirect3DDevice9, data: &[Vertex]) {
    unsafe {
        let _ = device.DrawPrimitiveUP(
            D3DPT_TRIANGLELIST,
            (data.len() / 3) as u32,
            data.as_ptr() as *const c_void,
            size_of::<Vertex>() as u32,
        );
    }
}

unsafe fn modulate_stage(device: &IDirect3DDevice9) {
    unsafe {
        let _ = device.SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_MODULATE.0 as u32);
        let _ = device.SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
        let _ = device.SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
        let _ = device.SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_MODULATE.0 as u32);
        let _ = device.SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
        let _ = device.SetTextureStageState(0, D3DTSS_ALPHAARG2, D3DTA_DIFFUSE);
    }
}

unsafe fn apply_states(device: &IDirect3DDevice9) {
    unsafe {
        let _ = device.SetVertexShader(None);
        let _ = device.SetPixelShader(None);
        let _ = device.SetFVF(FVF);
        let _ = device.SetRenderState(D3DRS_ZENABLE, 0);
        let _ = device.SetRenderState(D3DRS_LIGHTING, 0);
        let _ = device.SetRenderState(D3DRS_FOGENABLE, 0);
        let _ = device.SetRenderState(D3DRS_STENCILENABLE, 0);
        let _ = device.SetRenderState(D3DRS_SCISSORTESTENABLE, 0);
        let _ = device.SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE.0 as u32);
        let _ = device.SetRenderState(D3DRS_ALPHABLENDENABLE, 1);
        let _ = device.SetRenderState(D3DRS_SRCBLEND, D3DBLEND_SRCALPHA.0 as u32);
        let _ = device.SetRenderState(D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA.0 as u32);
    }
}

#[allow(clippy::too_many_arguments)]
fn push_quad(
    out: &mut Vec<Vertex>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: u32,
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
) {
    let make = |x: f32, y: f32, u: f32, v: f32| Vertex {
        x: x - 0.5,
        y: y - 0.5,
        z: 0.0,
        rhw: 1.0,
        color,
        u,
        v,
    };
    let top_left = make(x, y, u0, v0);
    let top_right = make(x + w, y, u1, v0);
    let bottom_right = make(x + w, y + h, u1, v1);
    let bottom_left = make(x, y + h, u0, v1);

    out.extend_from_slice(&[top_left, top_right, bottom_right]);
    out.extend_from_slice(&[top_left, bottom_right, bottom_left]);
}
