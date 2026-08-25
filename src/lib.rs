//! piscatio — inject-DLL авторыбалки для Terraria 1.4.5.6 (x86).
//!
//! Инжектор внешний и любой: поддержаны оба контракта — обычный `DllMain`
//! при LoadLibrary-инжекте и экспорты `Start` / `Stop` для manual-map,
//! где `DllMain` может не вызываться.

mod app;
mod clr;
mod config;
mod game;
pub mod logging;
mod overlay;

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use windows::Win32::Foundation::{HMODULE, MAX_PATH};
use windows::Win32::System::LibraryLoader::{
    DisableThreadLibraryCalls, FreeLibraryAndExitThread, GetModuleFileNameW,
};

const DLL_PROCESS_ATTACH: u32 = 1;
const DLL_PROCESS_DETACH: u32 = 0;

pub(crate) static SHUTDOWN: AtomicBool = AtomicBool::new(false);
pub(crate) static UNLOAD_REQUESTED: AtomicBool = AtomicBool::new(false);
pub(crate) static SHOW_UI: AtomicBool = AtomicBool::new(false);

static STARTED: AtomicBool = AtomicBool::new(false);
static MODULE: AtomicUsize = AtomicUsize::new(0);

/// Папка, куда кладём лог и конфиг: рядом с DLL, а при manual-map,
/// когда своего модуля у нас нет, — рядом с игрой.
fn base_dir() -> PathBuf {
    let handle = MODULE.load(Ordering::Relaxed);
    let mut buffer = [0u16; MAX_PATH as usize];

    let module = if handle == 0 {
        None
    } else {
        Some(HMODULE(handle as *mut c_void))
    };
    let len = unsafe { GetModuleFileNameW(module, &mut buffer) } as usize;

    if len > 0 && len < buffer.len() {
        let path = PathBuf::from(String::from_utf16_lossy(&buffer[..len]));
        if let Some(parent) = path.parent() {
            return parent.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Запуск рабочего потока. Идемпотентен: повторный вызов ничего не делает.
fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    SHUTDOWN.store(false, Ordering::SeqCst);

    std::thread::spawn(|| {
        let dir = base_dir();
        logging::init(dir.clone());

        // Паника через FFI-границу убивает игру — гасим её здесь.
        let result = std::panic::catch_unwind(|| app::run(dir));
        if result.is_err() {
            log!("паника в рабочем потоке, поток остановлен");
        }

        STARTED.store(false, Ordering::SeqCst);

        // Детуры обязаны быть сняты до выгрузки: иначе следующий кадр
        // прыгнет по адресу уже отображённого кода.
        overlay::uninstall();
        std::thread::sleep(std::time::Duration::from_millis(250));

        if UNLOAD_REQUESTED.swap(false, Ordering::SeqCst) {
            let handle = MODULE.load(Ordering::Relaxed);
            if handle != 0 {
                log!("выгружаю DLL");
                unsafe { FreeLibraryAndExitThread(HMODULE(handle as *mut c_void), 0) };
            } else {
                log!("выгрузка невозможна: модуль не зарегистрирован (manual-map)");
            }
        }
    });
}

fn stop() {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Точки входа
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *mut c_void) -> i32 {
    match reason {
        DLL_PROCESS_ATTACH => {
            MODULE.store(module.0 as usize, Ordering::Relaxed);
            unsafe {
                let _ = DisableThreadLibraryCalls(module);
            }
            start();
        }
        DLL_PROCESS_DETACH => stop(),
        _ => {}
    }
    1
}

/// Точка входа для manual-map инжекторов, которые не зовут `DllMain`.
#[unsafe(no_mangle)]
pub extern "system" fn Start() {
    start();
}

/// Остановить рабочий поток, не выгружая модуль.
#[unsafe(no_mangle)]
pub extern "system" fn Stop() {
    stop();
}
