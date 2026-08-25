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
use xnb::GameFont;

const D3D_SDK_VERSION: u32 = 32;
const SLOT_RESET: usize = 16;
const SLOT_PRESENT: usize = 17;

const FVF: u32 = D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1;

// Палитра под инвентарные панели Terraria.
const COLOR_BORDER: u32 = 0xFF_1B1B38;
const COLOR_FRAME: u32 = 0xFF_5A5CB8;
const COLOR_BACK: u32 = 0xE6_2E3070;
const COLOR_TITLE: u32 = 0xFF_FFD75E;
const COLOR_TEXT: u32 = 0xFF_E4E4F2;
const COLOR_LABEL: u32 = 0xFF_9FA3D8;

const PANEL_TOP: f32 = 16.0;
const PANEL_MIN_W: f32 = 280.0;
const PADDING: f32 = 12.0;
const COLUMN_GAP: f32 = 14.0;

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
static STATE_BLOCK: AtomicUsize = AtomicUsize::new(0);

/// Пары «подпись — значение»: шрифт игры пропорциональный,
/// поэтому колонки выравниваем координатами, а не пробелами.
static SNAPSHOT: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static FONT: OnceLock<Option<GameFont>> = OnceLock::new();

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

/// Обновить строки панели: слева подпись, справа значение.
pub fn set_lines(lines: Vec<(String, String)>) {
    if let Ok(mut slot) = SNAPSHOT.lock() {
        *slot = lines;
    }
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
    if FONT_TEXTURE.load(Ordering::SeqCst) != 0 || STATE_BLOCK.load(Ordering::SeqCst) != 0 {
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

/// `Content/Fonts/Mouse_Text.xnb` рядом с исполняемым файлом игры.
fn game_font_path() -> Option<std::path::PathBuf> {
    let mut buffer = [0u16; 260];
    let len = unsafe { GetModuleFileNameW(None, &mut buffer) } as usize;
    if len == 0 || len >= buffer.len() {
        return None;
    }
    let exe = std::path::PathBuf::from(String::from_utf16_lossy(&buffer[..len]));
    let dir = exe.parent()?;
    let path = dir.join("Content").join("Fonts").join("Mouse_Text.xnb");
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
    let block = STATE_BLOCK.swap(0, Ordering::SeqCst);
    if block != 0 {
        unsafe { drop(IDirect3DStateBlock9::from_raw(block as *mut c_void)) };
    }
    if texture != 0 || block != 0 {
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
    unsafe {
        let mut texture: Option<IDirect3DTexture9> = None;
        device
            .CreateTexture(
                atlas.width,
                atlas.height,
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
        for row in 0..atlas.height as usize {
            let destination =
                (locked.pBits as *mut u8).add(row * locked.Pitch as usize) as *mut u32;
            let source = atlas.pixels.as_ptr().add(row * atlas.width as usize);
            std::ptr::copy_nonoverlapping(source, destination, atlas.width as usize);
        }
        texture.UnlockRect(0).ok()?;
        Some(texture)
    }
}

// ---------------------------------------------------------------------------
// Рендер
// ---------------------------------------------------------------------------

unsafe fn draw(raw: *mut c_void) {
    let Some(device) = (unsafe { IDirect3DDevice9::from_raw_borrowed(&raw) }) else {
        return;
    };
    if !ensure_resources(device) {
        return;
    }

    let texture_ptr = FONT_TEXTURE.load(Ordering::Relaxed);
    let block_ptr = STATE_BLOCK.load(Ordering::Relaxed);
    if texture_ptr == 0 || block_ptr == 0 {
        return;
    }
    let texture_raw = texture_ptr as *mut c_void;
    let block_raw = block_ptr as *mut c_void;
    let (Some(texture), Some(block)) = (unsafe {
        (
            IDirect3DTexture9::from_raw_borrowed(&texture_raw),
            IDirect3DStateBlock9::from_raw_borrowed(&block_raw),
        )
    }) else {
        return;
    };

    let lines = SNAPSHOT.lock().map(|l| l.clone()).unwrap_or_default();
    let font = FONT.get().and_then(|f| f.as_ref());

    let line_height = font.map(|f| f.line_height).unwrap_or(20.0) + 2.0;
    let body = lines.len().max(1) as f32 * line_height;
    let panel_h = PADDING * 2.0 + line_height + 6.0 + body;

    // Ширина колонок: подписи выравниваем по самой длинной.
    let label_w = font
        .map(|f| {
            lines
                .iter()
                .map(|(label, _)| measure(f, label))
                .fold(0.0f32, f32::max)
        })
        .unwrap_or(0.0);
    let value_w = font
        .map(|f| {
            lines
                .iter()
                .map(|(_, value)| measure(f, value))
                .chain(std::iter::once(measure(f, "piscatio")))
                .fold(0.0f32, f32::max)
        })
        .unwrap_or(0.0);
    let column_gap = if label_w > 0.0 { COLUMN_GAP } else { 0.0 };
    let panel_w = (label_w + column_gap + value_w + PADDING * 2.0).max(PANEL_MIN_W);

    // Панель по центру сверху — позиция считается от текущего вьюпорта.
    let screen_w = unsafe {
        let mut viewport = D3DVIEWPORT9::default();
        match device.GetViewport(&mut viewport) {
            Ok(()) => viewport.Width as f32,
            Err(_) => 1280.0,
        }
    };
    let panel_x = ((screen_w - panel_w) * 0.5).max(0.0).floor();
    let panel_y = PANEL_TOP;

    let mut quads: Vec<Vertex> = Vec::with_capacity(24);
    push_rect(
        &mut quads,
        panel_x - 2.0,
        panel_y - 2.0,
        panel_w + 4.0,
        panel_h + 4.0,
        COLOR_BORDER,
    );
    push_rect(
        &mut quads,
        panel_x - 1.0,
        panel_y - 1.0,
        panel_w + 2.0,
        panel_h + 2.0,
        COLOR_FRAME,
    );
    push_rect(&mut quads, panel_x, panel_y, panel_w, panel_h, COLOR_BACK);

    let mut glyphs: Vec<Vertex> = Vec::with_capacity(1024);
    if let Some(loaded) = font {
        let mut y = panel_y + PADDING;
        push_text(
            &mut glyphs,
            loaded,
            panel_x + PADDING,
            y,
            "piscatio",
            COLOR_TITLE,
        );
        y += line_height + 6.0;
        let value_x = panel_x + PADDING + label_w + column_gap;
        for (label, value) in &lines {
            if !label.is_empty() {
                push_text(
                    &mut glyphs,
                    loaded,
                    panel_x + PADDING,
                    y,
                    label,
                    COLOR_LABEL,
                );
            }
            push_text(&mut glyphs, loaded, value_x, y, value, COLOR_TEXT);
            y += line_height;
        }
    }

    unsafe {
        // В Present сцена уже закрыта, а DrawPrimitiveUP работает только
        // внутри сцены — открываем свою.
        if device.BeginScene().is_err() {
            return;
        }
        let _ = block.Capture();
        apply_states(device);

        if !quads.is_empty() {
            let _ = device.SetTexture(0, None);
            let _ = device.SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1.0 as u32);
            let _ = device.SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
            let _ = device.SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1.0 as u32);
            let _ = device.SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_DIFFUSE);
            let _ = device.DrawPrimitiveUP(
                D3DPT_TRIANGLELIST,
                (quads.len() / 3) as u32,
                quads.as_ptr() as *const c_void,
                size_of::<Vertex>() as u32,
            );
        }

        if !glyphs.is_empty() {
            let _ = device.SetTexture(0, texture);
            let _ = device.SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_MODULATE.0 as u32);
            let _ = device.SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
            let _ = device.SetTextureStageState(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
            let _ = device.SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_MODULATE.0 as u32);
            let _ = device.SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
            let _ = device.SetTextureStageState(0, D3DTSS_ALPHAARG2, D3DTA_DIFFUSE);
            let _ = device.DrawPrimitiveUP(
                D3DPT_TRIANGLELIST,
                (glyphs.len() / 3) as u32,
                glyphs.as_ptr() as *const c_void,
                size_of::<Vertex>() as u32,
            );
        }

        // Текстуру снимаем явно: если она останется в стейдже, игра
        // нарисует ей свои спрайты.
        let _ = device.SetTexture(0, None);
        let _ = block.Apply();
        let _ = device.EndScene();
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

fn push_rect(out: &mut Vec<Vertex>, x: f32, y: f32, w: f32, h: f32, color: u32) {
    push_quad(out, x, y, w, h, color, 0.0, 0.0, 0.0, 0.0);
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

/// Ширина строки в пикселях.
fn measure(font: &GameFont, text: &str) -> f32 {
    text.chars()
        .map(|ch| {
            font.glyphs
                .get(&ch)
                .map(|g| g.advance)
                .unwrap_or(font.space_advance)
        })
        .sum()
}

fn push_text(out: &mut Vec<Vertex>, font: &GameFont, x: f32, y: f32, text: &str, color: u32) {
    let mut pen = x;
    let tw = font.width as f32;
    let th = font.height as f32;
    for ch in text.chars() {
        let Some(glyph) = font.glyphs.get(&ch) else {
            pen += font.space_advance;
            continue;
        };
        if glyph.w > 0 && glyph.h > 0 {
            push_quad(
                out,
                (pen + glyph.off_x).round(),
                (y + glyph.off_y).round(),
                glyph.w as f32,
                glyph.h as f32,
                color,
                glyph.sx as f32 / tw,
                glyph.sy as f32 / th,
                (glyph.sx + glyph.w) as f32 / tw,
                (glyph.sy + glyph.h) as f32 / th,
            );
        }
        pen += glyph.advance;
    }
}
