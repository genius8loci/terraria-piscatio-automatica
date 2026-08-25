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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use minhook::MinHook;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D9::{
    D3DBLEND_INVSRCALPHA, D3DBLEND_SRCALPHA, D3DCREATE_SOFTWARE_VERTEXPROCESSING, D3DCULL_NONE,
    D3DDEVTYPE_HAL, D3DFMT_A8R8G8B8, D3DFMT_UNKNOWN, D3DFVF_DIFFUSE, D3DFVF_TEX1, D3DFVF_XYZRHW,
    D3DLOCKED_RECT, D3DPOOL_DEFAULT, D3DPRESENT_PARAMETERS, D3DPT_TRIANGLELIST,
    D3DRS_ALPHABLENDENABLE, D3DRS_CULLMODE, D3DRS_DESTBLEND, D3DRS_FOGENABLE, D3DRS_LIGHTING,
    D3DRS_SCISSORTESTENABLE, D3DRS_SRCBLEND, D3DRS_STENCILENABLE, D3DRS_ZENABLE, D3DSAMP_ADDRESSU,
    D3DSAMP_ADDRESSV, D3DSAMP_MAGFILTER, D3DSAMP_MINFILTER, D3DSAMP_MIPFILTER, D3DSBT_ALL,
    D3DSWAPEFFECT_DISCARD, D3DTA_DIFFUSE, D3DTA_TEXTURE, D3DTADDRESS_CLAMP, D3DTEXF_LINEAR,
    D3DTEXF_NONE, D3DTEXF_POINT, D3DTEXTUREFILTERTYPE, D3DTOP_MODULATE, D3DTSS_ALPHAARG1,
    D3DTSS_ALPHAARG2, D3DTSS_ALPHAOP, D3DTSS_COLORARG1, D3DTSS_COLORARG2, D3DTSS_COLOROP,
    D3DUSAGE_DYNAMIC, D3DVIEWPORT9, Direct3DCreate9, Direct3DCreate9Ex, IDirect3D9, IDirect3D9Ex,
    IDirect3DDevice9, IDirect3DDevice9Ex, IDirect3DStateBlock9, IDirect3DTexture9,
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

/// Палитра игры. Оттенки взяты из её же кода и текстур, поэтому
/// подбирать здесь почти нечего: панели и строки красятся белым,
/// собственный цвет у них внутри текстуры.
pub mod colors {
    /// Фон окна: `new Color(63, 82, 151) * 0.7f` из `UIPanel` — сквозь
    /// панель видно мир ровно так же, как в родном интерфейсе.
    pub const PANEL: u32 = 0xB3_3F5297;
    /// Обводка окна: `UIPanel.BorderColor` = чёрный.
    pub const PANEL_BORDER: u32 = 0xFF_000000;
    /// «Как есть»: текстура уже нужного цвета.
    pub const PLAIN: u32 = 0xFF_FFFFFF;

    // Кнопка — та же панель, но светлее фона окна, чтобы читалась как кнопка.
    pub const BUTTON: u32 = 0xC8_4A56A8;
    pub const BUTTON_HOVER: u32 = 0xE6_6472CE;
    pub const BUTTON_ACTIVE: u32 = 0xFF_7A88E0;

    /// Ползунок прокрутки в атласе белый — цвет ему задаём здесь.
    pub const HANDLE: u32 = 0xFF_7C8CD8;
    pub const HANDLE_HOVER: u32 = 0xFF_A8B6F0;

    /// Ячейка инвентаря: игра рисует её слегка прозрачной.
    pub const SLOT: u32 = 0xCC_FFFFFF;
    pub const SLOT_OFF: u32 = 0x80_FFFFFF;

    /// Жёлтый заголовков — тот же, что у игры в UI.
    pub const TITLE: u32 = 0xFF_FFE745;
    // Цвета редкости предметов из `ItemRarityColor`: ими игра красит имена
    // в подсказках, так что для разбивки заголовка они здесь уместны.
    /// Голубой, редкость 1.
    pub const RARE_BLUE: u32 = 0xFF_9696FF;
    /// Светло-фиолетовый, редкость 6.
    pub const RARE_PURPLE: u32 = 0xFF_D2A0FF;
    /// Зелёный, редкость 2 — им подсвечиваем отобранное в фильтре.
    pub const RARE_GREEN: u32 = 0xFF_96FF96;
    /// Красный, редкость 4 — им перечёркиваем отвергнутое.
    pub const RARE_RED: u32 = 0xFF_FF9696;
    pub const TEXT: u32 = 0xFF_FFFFFF;
    pub const MUTED: u32 = 0xFF_A2A8CE;
    pub const VALUE: u32 = 0xFF_FFE745;
    pub const ON: u32 = 0xFF_8CE79A;
    /// Иконка выключенного зелья приглушается.
    pub const ICON_OFF: u32 = 0x99_FFFFFF;
    /// Отвергнутый предмет тускнеет — как недоступное в меню дублирования.
    pub const ICON_DENIED: u32 = 0xCC_C4C4D0;
    /// Крестик поверх него.
    pub const CROSS: u32 = 0xE6_FF4040;
}

/// Ячейка инвентаря у игры — текстура в 52 пикселя, и иконка внутри неё
/// рисуется относительно этого размера. Крупнее 32 пикселей игра ужимает.
const SLOT_TEXTURE_SIZE: f32 = 52.0;
const SLOT_ICON_LIMIT: f32 = 32.0;
/// Насколько игра уменьшает ячейки инвентаря: `Main.inventoryScale`.
/// Тот же множитель нужен нам, чтобы иконки совпадали с игровыми один в один.
pub const INVENTORY_SCALE: f32 = 0.85;

/// Обводка текста: игра рисует её в два пикселя при масштабе 1.
const SHADOW: f32 = 2.0;
const SHADOW_COLOR: u32 = 0xFF_000000;

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
/// Девайс, увиденный в `Present`: детуру `DrawCursor` его взять неоткуда.
static DEVICE: AtomicUsize = AtomicUsize::new(0);
/// Панель уже нарисована детуром курсора в этом кадре.
static DREW_IN_CURSOR: AtomicBool = AtomicBool::new(false);
/// Сколько раз панель нарисовалась из детура — видно в строке статуса.
pub static CURSOR_DRAWS: AtomicU32 = AtomicU32::new(0);

// Ввод снимается только в `Present`. Детур `DrawCursor` — голый хук на
// managed-метод, и лезть из него обратно в CLR незачем: положение мыши
// возрастом в один кадр глазу неразличимо, а лишний повод для падения
// внутри чужого кадра — нет.
static MOUSE_X: AtomicI32 = AtomicI32::new(-1);
static MOUSE_Y: AtomicI32 = AtomicI32::new(-1);
/// Нажатие по фронту, ещё не отданное отрисовке.
static MOUSE_CLICK: AtomicBool = AtomicBool::new(false);
/// Кнопка держится — по этому тянут ползунок прокрутки.
static MOUSE_DOWN: AtomicBool = AtomicBool::new(false);
/// Щелчки колеса, ещё не отданные отрисовке.
static WHEEL: AtomicI32 = AtomicI32::new(0);
/// Курсор над нашим окном — результат последней отрисовки.
static OVER_UI: AtomicBool = AtomicBool::new(false);
/// Предмет под курсором; `0` — ничего. По нему показывается подсказка игры.
static HOVER_ITEM: AtomicI32 = AtomicI32::new(0);
/// Курсор стоит в строке поиска: клавиши сейчас про текст, и хоткеи
/// рабочего потока трогать нельзя — иначе Delete выгрузит DLL при наборе.
static TYPING: AtomicBool = AtomicBool::new(false);
/// Масштаб интерфейса игры, битами `f32`: атомика для плавающих нет.
static UI_SCALE: AtomicU32 = AtomicU32::new(0);
/// Отрисовка уронила панику: дальше не рисуем, но игру не роняем.
static BROKEN: AtomicBool = AtomicBool::new(false);
static ICON_TEXTURE: AtomicUsize = AtomicUsize::new(0);
static STATE_BLOCK: AtomicUsize = AtomicUsize::new(0);

/// Пары «подпись — значение»: шрифт игры пропорциональный,
/// поэтому колонки выравниваем координатами, а не пробелами.
static FONT: OnceLock<Option<GameFont>> = OnceLock::new();
static ICONS: Mutex<Option<IconAtlas>> = Mutex::new(None);
/// Набор предметов, под который собран текущий атлас: id и число кадров.
static ICON_ITEMS: Mutex<Vec<(i32, u32)>> = Mutex::new(Vec::new());

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
    DEVICE.store(0, Ordering::SeqCst);
    DREW_IN_CURSOR.store(false, Ordering::SeqCst);
    OVER_UI.store(false, Ordering::SeqCst);
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

    // Девайс могли не сбросить, а пересоздать. Тогда наши ресурсы
    // принадлежат покойнику: освобождать их через мёртвый девайс нельзя,
    // остаётся забыть указатели и дать пересоздать заново.
    let previous = DEVICE.swap(device as usize, Ordering::SeqCst);
    if previous != 0 && previous != device as usize {
        forget_resources();
    }

    if ACTIVE.load(Ordering::Relaxed) {
        crate::FRAME.fetch_add(1, Ordering::Relaxed);
        if !FIRST_FRAME_LOGGED.swap(true, Ordering::SeqCst) {
            crate::log!("оверлей: первый кадр перехвачен, рендер работает");
        }

        // Ввод и масштаб глазами игры — единственное место, где мы трогаем
        // CLR из кадра. Детур курсора читает уже готовые значения.
        let (mx, my, down) = crate::input::cursor().unwrap_or((-1, -1, false));
        feed_input(mx, my, down, crate::input::wheel());
        if let Some(scale) = crate::input::ui_scale() {
            UI_SCALE.store(scale.to_bits(), Ordering::Relaxed);
        }

        // Ресурсы заводим только здесь. В детуре курсора мы внутри чужой
        // открытой сцены, и создавать там текстуры незачем: `Present`
        // случается каждый кадр и успевает подготовить всё заранее.
        let shown = SHOW_UI.load(Ordering::Relaxed);
        if shown {
            prepare(device);
        } else {
            OVER_UI.store(false, Ordering::Relaxed);
            HOVER_ITEM.store(0, Ordering::Relaxed);
            // Иначе нажатие, сделанное при скрытой панели, сработает,
            // как только её покажут.
            MOUSE_CLICK.store(false, Ordering::Relaxed);
            WHEEL.store(0, Ordering::Relaxed);
        }
        // Пока курсор над окном, игра не должна считать клик игровым.
        // Значение посчитал детур курсора чуть раньше в этом же кадре.
        if OVER_UI.load(Ordering::Relaxed) {
            crate::input::claim_mouse_interface();
        }

        // Обычно панель рисует детур `DrawCursor` — там она ложится под
        // курсор игры. Сюда доходим, только если детур не встал или игра
        // в этом кадре курсор не рисовала: тогда рисуем сами, вместе с ним.
        let already = DREW_IN_CURSOR.swap(false, Ordering::SeqCst);
        if !already && shown && !BROKEN.load(Ordering::Relaxed) {
            guarded(device, true);
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
    }
    let block = STATE_BLOCK.swap(0, Ordering::SeqCst);
    if block != 0 {
        unsafe { drop(IDirect3DStateBlock9::from_raw(block as *mut c_void)) };
    }
    if texture != 0 || block != 0 || icons != 0 {
        crate::log!("оверлей: ресурсы освобождены ({reason})");
    }
}

/// Забывает ресурсы, не освобождая: владелец мёртв, и `Release` по его
/// объектам — обращение к освобождённой памяти. Утечка здесь безопаснее.
fn forget_resources() {
    let font = FONT_TEXTURE.swap(0, Ordering::SeqCst);
    let icons = ICON_TEXTURE.swap(0, Ordering::SeqCst);
    let block = STATE_BLOCK.swap(0, Ordering::SeqCst);
    if font != 0 || icons != 0 || block != 0 {
        crate::log!("оверлей: девайс сменился, старые ресурсы брошены");
    }
}

fn ensure_resources(device: &IDirect3DDevice9) -> bool {
    if FONT_TEXTURE.load(Ordering::Relaxed) != 0 && STATE_BLOCK.load(Ordering::Relaxed) != 0 {
        return true;
    }
    // Если уцелела половина пары, освобождаем её: иначе она утечёт
    // и не даст устройству сделать Reset.
    release_resources("пересоздание");

    // Шрифт грузим лениво: пробнику оверлея `install` не нужен.
    let Some(atlas) = FONT.get_or_init(load_font).as_ref() else {
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

/// Накопитель геометрии кадра. Партии две: всё оформление идёт одной
/// текстурой-атласом, текст — другой. Внутри партии слои ложатся в порядке
/// вызовов, поэтому отдельная партия под заливки не нужна.
pub struct Painter<'a> {
    ui: &'a mut Vec<Vertex>,
    text: &'a mut Vec<Vertex>,
    font: Option<&'a GameFont>,
    atlas: Option<&'a IconAtlas>,
    pub scale: f32,
}

impl<'a> Painter<'a> {
    /// Заливка: берём белый квадратик из атласа, чтобы не разбивать партию.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: u32) {
        self.stretch(icons::WHITE, x, y, w, h, color);
    }

    /// Текстура целиком, растянутая в прямоугольник.
    pub fn stretch(&mut self, id: i32, x: f32, y: f32, w: f32, h: f32, color: u32) {
        let Some(atlas) = self.atlas else {
            return;
        };
        let Some(rect) = atlas.get(id) else {
            return;
        };
        let (tw, th) = (atlas.width as f32, atlas.height as f32);
        push_quad(
            self.ui,
            x,
            y,
            w,
            h,
            color,
            rect.x as f32 / tw,
            rect.y as f32 / th,
            (rect.x + rect.w) as f32 / tw,
            (rect.y + rect.h) as f32 / th,
        );
    }

    /// Девятичастная нарезка, как рисует свои панели сама игра: углы идут
    /// как есть, края тянутся вдоль, середина заполняет остальное.
    /// `inset` — ширина рамки в пикселях исходной текстуры.
    #[allow(clippy::too_many_arguments)]
    pub fn nine_slice(&mut self, id: i32, x: f32, y: f32, w: f32, h: f32, inset: f32, color: u32) {
        let Some(atlas) = self.atlas else {
            return;
        };
        let Some(rect) = atlas.get(id) else {
            return;
        };
        let (tw, th) = (atlas.width as f32, atlas.height as f32);
        // В исходнике рамка не может съесть больше половины текстуры,
        // на экране — больше половины прямоугольника.
        let su = inset.min(rect.w as f32 * 0.5);
        let sv = inset.min(rect.h as f32 * 0.5);
        let du = (inset * self.scale).round().min((w * 0.5).floor()).max(1.0);
        let dv = (inset * self.scale).round().min((h * 0.5).floor()).max(1.0);

        // Тройки «экран: начало, длина; текстура: начало, длина».
        let cols = [
            (x, du, rect.x as f32, su),
            (
                x + du,
                w - du * 2.0,
                rect.x as f32 + su,
                rect.w as f32 - su * 2.0,
            ),
            (x + w - du, du, (rect.x + rect.w) as f32 - su, su),
        ];
        let rows = [
            (y, dv, rect.y as f32, sv),
            (
                y + dv,
                h - dv * 2.0,
                rect.y as f32 + sv,
                rect.h as f32 - sv * 2.0,
            ),
            (y + h - dv, dv, (rect.y + rect.h) as f32 - sv, sv),
        ];

        for (cx, cw, cu, cuw) in cols {
            if cw <= 0.0 || cuw <= 0.0 {
                continue;
            }
            for (ry, rh, rv, rvh) in rows {
                if rh <= 0.0 || rvh <= 0.0 {
                    continue;
                }
                push_quad(
                    self.ui,
                    cx,
                    ry,
                    cw,
                    rh,
                    color,
                    cu / tw,
                    rv / th,
                    (cu + cuw) / tw,
                    (rv + rvh) / th,
                );
            }
        }
    }

    /// Спрайт из атласа в натуральную величину с учётом масштаба.
    pub fn sprite(&mut self, item: i32, x: f32, y: f32, color: u32) {
        let Some(atlas) = self.atlas else {
            return;
        };
        let Some(rect) = atlas.get(item) else {
            return;
        };
        self.stretch(
            item,
            x.round(),
            y.round(),
            rect.w as f32 * self.scale,
            rect.h as f32 * self.scale,
            color,
        );
    }

    /// Строка с чёрной обводкой — так игра рисует весь свой текст
    /// (`ChatManager.DrawColorCodedStringWithShadow`): четыре чёрных прохода
    /// со сдвигом по осям и цветной поверх. Без обводки шрифт тот же самый,
    /// но выглядит тусклым и чужим: он рассчитан на неё.
    pub fn text(&mut self, x: f32, y: f32, value: &str, color: u32) {
        let shadow = (SHADOW * self.scale).round().max(1.0);
        for (dx, dy) in [(-shadow, 0.0), (shadow, 0.0), (0.0, -shadow), (0.0, shadow)] {
            self.glyphs(x + dx, y + dy, value, SHADOW_COLOR);
        }
        self.glyphs(x, y, value, color);
    }

    /// Один проход по глифам, без обводки.
    fn glyphs(&mut self, x: f32, y: f32, value: &str, color: u32) {
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

    /// Курсор для строки, вертикально выровненной по середине коробки.
    /// Считаем по видимой части строки, а не по межстрочному интервалу:
    /// он у шрифта игры заметно больше, и текст прижимался к верху.
    fn text_baseline(&self, y: f32, h: f32) -> f32 {
        let Some(font) = self.font else {
            return y;
        };
        let ink = font.ink_height() * self.scale;
        (y + (h - ink) * 0.5 - font.ink_top * self.scale).round()
    }

    /// Строка по центру коробки — и по вертикали, и по горизонтали.
    pub fn text_centered(&mut self, x: f32, y: f32, w: f32, h: f32, value: &str, color: u32) {
        let width = self.measure(value);
        let baseline = self.text_baseline(y, h);
        self.text(x + (w - width) * 0.5, baseline, value, color);
    }

    /// Строка у левого края коробки, по центру по вертикали.
    pub fn text_left(&mut self, x: f32, y: f32, h: f32, value: &str, color: u32) {
        let baseline = self.text_baseline(y, h);
        self.text(x, baseline, value, color);
    }

    /// Строка у правого края коробки, по центру по вертикали.
    pub fn text_right(&mut self, x: f32, y: f32, w: f32, h: f32, value: &str, color: u32) {
        let width = self.measure(value);
        let baseline = self.text_baseline(y, h);
        self.text(x + w - width, baseline, value, color);
    }

    /// Иконка предмета в ячейке — по правилам самой игры (`ItemSlot.Draw`):
    /// картинка больше 32 пикселей ужимается до 32 по большей стороне,
    /// меньшая идёт как есть, и всё это умножается на размер ячейки
    /// относительно её текстуры в 52 пикселя.
    pub fn icon(&mut self, item: i32, x: f32, y: f32, w: f32, h: f32, color: u32) {
        let Some(atlas) = self.atlas else {
            return;
        };
        let Some(rect) = atlas.get(item) else {
            return;
        };
        let longest = rect.w.max(rect.h) as f32;
        let fit = if longest > SLOT_ICON_LIMIT {
            SLOT_ICON_LIMIT / longest
        } else {
            1.0
        };
        let cell = fit * (w / SLOT_TEXTURE_SIZE);
        let dw = rect.w as f32 * cell;
        let dh = rect.h as f32 * cell;
        self.stretch(
            item,
            (x + (w - dw) * 0.5).round(),
            (y + (h - dh) * 0.5).round(),
            dw.round(),
            dh.round(),
            color,
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
/// Второе число в паре — сколько кадров анимации в картинке предмета.
pub fn set_icon_items(items: Vec<(i32, u32)>) {
    if let Ok(mut slot) = ICON_ITEMS.lock() {
        if *slot == items {
            return;
        }
        *slot = items;
    }
    ICONS_STALE.store(true, Ordering::SeqCst);
}

/// Держит атлас в актуальном виде. Пиксели собираются один раз: после
/// `Reset` теряется только текстура, перечитывать файлы незачем.
fn ensure_icons(device: &IDirect3DDevice9) {
    if ICONS_STALE.swap(false, Ordering::SeqCst) {
        if let Ok(mut slot) = ICONS.lock() {
            *slot = None;
        }
        let previous = ICON_TEXTURE.swap(0, Ordering::SeqCst);
        if previous != 0 {
            unsafe { drop(IDirect3DTexture9::from_raw(previous as *mut c_void)) };
        }
    }

    let Ok(mut slot) = ICONS.lock() else {
        return;
    };
    if slot.is_none() {
        let Some(content) = content_dir() else {
            return;
        };
        let items = ICON_ITEMS.lock().map(|i| i.clone()).unwrap_or_default();
        let Some(atlas) = icons::build(&content, &items) else {
            crate::log!("оверлей: атлас собрать не вышло");
            return;
        };
        crate::log!(
            "оверлей: атлас {}x{}, картинок {}",
            atlas.width,
            atlas.height,
            atlas.len()
        );
        *slot = Some(atlas);
    }

    if ICON_TEXTURE.load(Ordering::Relaxed) == 0 {
        let Some(atlas) = slot.as_ref() else {
            return;
        };
        if let Some(texture) = create_texture(device, atlas.width, atlas.height, &atlas.pixels) {
            ICON_TEXTURE.store(texture.into_raw() as usize, Ordering::SeqCst);
        }
    }
}

/// Принимает ввод, снятый с игры. Отрисовка берёт координаты отсюда, а не
/// из CLR: детур курсора идёт по чужому кадру, и лишний вызов в рантайм
/// оттуда — лишний повод упасть. Нажатие запоминается по фронту и живёт
/// до ближайшей отрисовки, которая его и заберёт.
pub(crate) fn feed_input(x: i32, y: i32, down: bool, wheel: i32) {
    MOUSE_X.store(x, Ordering::Relaxed);
    MOUSE_Y.store(y, Ordering::Relaxed);
    MOUSE_DOWN.store(down, Ordering::Relaxed);
    if down && !MOUSE_WAS_DOWN.swap(down, Ordering::Relaxed) {
        MOUSE_CLICK.store(true, Ordering::Relaxed);
    } else if !down {
        MOUSE_WAS_DOWN.store(false, Ordering::Relaxed);
    }
    if wheel != 0 {
        WHEEL.fetch_add(wheel, Ordering::Relaxed);
    }
}

/// Масштаб интерфейса игры. Ноль означает «ещё не читали» — до подключения
/// к игре и в пробнике; тогда считаем, что масштаб единичный.
pub(crate) fn ui_scale() -> f32 {
    let bits = UI_SCALE.load(Ordering::Relaxed);
    if bits == 0 {
        return 1.0;
    }
    let scale = f32::from_bits(bits);
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// Задаёт масштаб интерфейса вручную — нужно пробнику, у которого игры нет.
#[allow(dead_code)]
pub(crate) fn set_ui_scale(scale: f32) {
    UI_SCALE.store(scale.to_bits(), Ordering::Relaxed);
}

/// Зовётся из детура `Main.DrawCursor`, с игрового потока и внутри кадра.
/// Здесь весь интерфейс игры уже выгружен на экран, а курсор ещё нет.
pub fn on_draw_cursor() {
    if !ACTIVE.load(Ordering::Relaxed)
        || !SHOW_UI.load(Ordering::Relaxed)
        || BROKEN.load(Ordering::Relaxed)
    {
        return;
    }
    let raw = DEVICE.load(Ordering::Relaxed);
    // Ресурсов ещё нет (первый кадр или только что был `Reset`) — пропускаем
    // ход: их заведёт `Present`, он же в этом кадре и нарисует.
    if raw == 0 || !resources_ready() || DREW_IN_CURSOR.swap(true, Ordering::SeqCst) {
        return;
    }
    CURSOR_DRAWS.fetch_add(1, Ordering::Relaxed);
    guarded(raw as *mut c_void, false);

    // Подсказку показываем руками игры — ту же самую, что в инвентаре.
    // Её рисует `DrawPendingMouseText` в самом конце интерфейса, уже после
    // нас, поэтому просить надо отсюда, а не из `Present`: там поздно.
    let item = HOVER_ITEM.load(Ordering::Relaxed);
    if item > 0 {
        let _ = catch_unwind(AssertUnwindSafe(|| crate::input::show_item_tooltip(item)));
    }

    // Набор в строке поиска игра разбирает сама — оттуда же, откуда это
    // делают её собственные поля ввода, то есть из отрисовки интерфейса.
    if TYPING.load(Ordering::Relaxed) {
        let _ = catch_unwind(AssertUnwindSafe(pump_search_text));
    }
}

/// Забирает у игры новое значение строки поиска.
fn pump_search_text() {
    let Ok(mut guard) = UI.lock() else {
        return;
    };
    let Some(state) = guard.as_mut() else {
        return;
    };
    let Some(next) = crate::input::edit_text(&state.search) else {
        return;
    };
    if next != state.search {
        state.search = next;
        // Список стал другим — показывать его с прежней прокрутки незачем.
        state.filter_row = 0;
    }
}

/// Идёт набор в строке поиска: хоткеи трогать нельзя.
pub fn is_typing() -> bool {
    TYPING.load(Ordering::Relaxed)
}

/// Раскрывает или сворачивает панель — то же, что клик по стрелке.
/// Сама стрелка при этом остаётся на месте.
pub fn toggle_expanded() {
    let Ok(mut guard) = UI.lock() else {
        return;
    };
    let state = guard.get_or_insert_with(ui::UiState::default);
    state.expanded = !state.expanded;
    if !state.expanded {
        state.search_focus = false;
    }
    crate::log!(
        "панель {}",
        if state.expanded {
            "раскрыта"
        } else {
            "свёрнута"
        }
    );
}

/// Отрисовка под `catch_unwind`: паника внутри чужого кадра убьёт игру.
/// Первая же паника гасит панель насовсем — молча повторять её каждый кадр
/// хуже, чем остаться без интерфейса, а в логе будет видно, что случилось.
fn guarded(device: *mut c_void, own_cursor: bool) {
    let result = catch_unwind(AssertUnwindSafe(|| unsafe { draw(device, own_cursor) }));
    if result.is_err() && !BROKEN.swap(true, Ordering::SeqCst) {
        crate::log!("оверлей: паника при отрисовке, панель отключена");
    }
}

/// `own_cursor` — рисовать ли курсор самим. Из детура не надо: игра
/// нарисует свой сразу после нас.
/// Заводит текстуры и блок состояния, если их нет. Зовётся только из
/// `Present`, вне чужой сцены.
pub(crate) fn prepare(raw: *mut c_void) {
    let Some(device) = (unsafe { IDirect3DDevice9::from_raw_borrowed(&raw) }) else {
        return;
    };
    if ensure_resources(device) {
        ensure_icons(device);
    }
}

fn resources_ready() -> bool {
    FONT_TEXTURE.load(Ordering::Relaxed) != 0 && STATE_BLOCK.load(Ordering::Relaxed) != 0
}

pub(crate) unsafe fn draw(raw: *mut c_void, own_cursor: bool) {
    let Some(device) = (unsafe { IDirect3DDevice9::from_raw_borrowed(&raw) }) else {
        return;
    };
    if !resources_ready() {
        return;
    }

    let mut ui_quads: Vec<Vertex> = Vec::with_capacity(2048);
    let mut text: Vec<Vertex> = Vec::with_capacity(2048);

    let screen = unsafe {
        let mut viewport = D3DVIEWPORT9::default();
        match device.GetViewport(&mut viewport) {
            Ok(()) => (viewport.Width as f32, viewport.Height as f32),
            Err(_) => (1280.0, 720.0),
        }
    };

    // Ввод снят в `Present`; нажатие и колесо забираем, чтобы не сработали
    // дважды, а удержание кнопки читаем как есть — по нему тянут ползунок.
    let input = ui::Input {
        x: MOUSE_X.load(Ordering::Relaxed) as f32,
        y: MOUSE_Y.load(Ordering::Relaxed) as f32,
        clicked: MOUSE_CLICK.swap(false, Ordering::Relaxed),
        down: MOUSE_DOWN.load(Ordering::Relaxed),
        wheel: WHEEL.swap(0, Ordering::Relaxed),
    };

    let frame = {
        let font = FONT.get().and_then(|f| f.as_ref());
        let atlas_guard = ICONS.lock().ok();
        let atlas = atlas_guard.as_ref().and_then(|g| g.as_ref());
        let mut painter = Painter {
            ui: &mut ui_quads,
            text: &mut text,
            font,
            atlas,
            scale: 1.0,
        };
        let Ok(mut guard) = UI.lock() else {
            return;
        };
        let ui_state = guard.get_or_insert_with(ui::UiState::default);
        ui::build(
            &mut painter,
            ui_state,
            input,
            screen,
            ui_scale(),
            own_cursor,
        )
    };

    OVER_UI.store(frame.over_ui, Ordering::Relaxed);
    HOVER_ITEM.store(frame.hover_item, Ordering::Relaxed);
    TYPING.store(frame.typing, Ordering::Relaxed);

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
        // В `Present` сцена уже закрыта, а `DrawPrimitiveUP` работает только
        // внутри сцены — открываем свою. Из детура курсора мы, наоборот,
        // внутри чужой сцены: `BeginScene` там вернёт ошибку, и это нормально.
        let opened = device.BeginScene().is_ok();
        let _ = block.Capture();
        apply_states(device);

        if let Some(icons) = icon_texture.filter(|_| !ui_quads.is_empty()) {
            let _ = device.SetTexture(0, icons);
            modulate_stage(device);
            // Оформление — пиксельная графика: тянем её без сглаживания,
            // иначе на границах кусков атласа появляется кайма.
            sampler(device, D3DTEXF_POINT);
            draw_batch(device, &ui_quads);
        }

        if !text.is_empty() {
            let _ = device.SetTexture(0, font_texture);
            modulate_stage(device);
            sampler(device, D3DTEXF_LINEAR);
            draw_batch(device, &text);
        }

        // Текстуру снимаем явно: если она останется в стейдже, игра
        // нарисует ей свои спрайты.
        let _ = device.SetTexture(0, None);
        let _ = block.Apply();
        if opened {
            let _ = device.EndScene();
        }
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

/// Фильтрация и режим краёв: у чужого кадра они могли остаться любыми.
unsafe fn sampler(device: &IDirect3DDevice9, filter: D3DTEXTUREFILTERTYPE) {
    unsafe {
        let _ = device.SetSamplerState(0, D3DSAMP_MINFILTER, filter.0 as u32);
        let _ = device.SetSamplerState(0, D3DSAMP_MAGFILTER, filter.0 as u32);
        let _ = device.SetSamplerState(0, D3DSAMP_MIPFILTER, D3DTEXF_NONE.0 as u32);
        let _ = device.SetSamplerState(0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP.0 as u32);
        let _ = device.SetSamplerState(0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP.0 as u32);
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
