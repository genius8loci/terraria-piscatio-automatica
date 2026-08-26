//! Ловушка на аварийное завершение.
//!
//! Игра дважды умирала молча: ни окна ошибки, ни строчки в логе. Тихая
//! смерть без диалога — это нативное нарушение доступа или переполнение
//! стека, и по логу такое не отследить, потому что до записи дело не
//! доходит. Векторный обработчик срабатывает **до** раскрутки стека, так
//! что успевает записать, что и где случилось.
//!
//! Обработчик ничего не чинит и не глотает: пишет строку и отдаёт
//! исключение дальше по цепочке. Управляемые исключения .NET (`0xE0434352`)
//! и прочие «первого шанса» пропускаем молча — их в игре тысячи, и они
//! обрабатываются штатно.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};

use windows::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_ILLEGAL_INSTRUCTION, EXCEPTION_STACK_OVERFLOW,
};
use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_POINTERS, RemoveVectoredExceptionHandler,
};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};

/// Отдать исключение следующему обработчику.
const CONTINUE_SEARCH: i32 = 0;

static HANDLE: AtomicUsize = AtomicUsize::new(0);
/// Сколько записей уже сделали: лог не должен превратиться в поток.
static LOGGED: AtomicU32 = AtomicU32::new(0);
const MAX_LOGGED: u32 = 4;

// ---------------------------------------------------------------------------
// Чем мы были заняты
// ---------------------------------------------------------------------------
//
// По одному адресу в `clr.dll` не видно, чей вызов туда зашёл: рефлексию
// зовут оба наших потока. Поэтому каждый помечает, что именно он начал,
// и отметка снимается сама, когда вызов вернулся. В падении обе отметки
// попадают в строку — и «фантомное» падение перестаёт быть фантомным.

pub const STEP_NONE: u8 = 0;
pub const STEP_CLICK: u8 = 1;
pub const STEP_QUICK_STACK: u8 = 2;
pub const STEP_CHAT: u8 = 3;
pub const STEP_ITEM_TOOLTIP: u8 = 4;
pub const STEP_TEXT_TOOLTIP: u8 = 5;
pub const STEP_SEARCH_TEXT: u8 = 6;
pub const STEP_BOBBER: u8 = 7;
pub const STEP_STOCK: u8 = 8;
pub const STEP_POTIONS: u8 = 9;
pub const STEP_ROD: u8 = 10;
pub const STEP_LANG: u8 = 11;
pub const STEP_SOUND: u8 = 12;

fn step_name(step: u8) -> &'static str {
    match step {
        STEP_CLICK => "нажатие",
        STEP_QUICK_STACK => "раскладка по сундукам",
        STEP_CHAT => "строка в чат",
        STEP_ITEM_TOOLTIP => "подсказка предмета",
        STEP_TEXT_TOOLTIP => "текстовая подсказка",
        STEP_SEARCH_TEXT => "набор в строке поиска",
        STEP_BOBBER => "чтение поплавка",
        STEP_STOCK => "наживка и ячейки",
        STEP_POTIONS => "зелья",
        STEP_ROD => "удочка в руке",
        STEP_LANG => "язык игры",
        STEP_SOUND => "звук квестовой рыбы",
        _ => "ничего",
    }
}

static GAME_STEP: AtomicU8 = AtomicU8::new(STEP_NONE);
static WORKER_STEP: AtomicU8 = AtomicU8::new(STEP_NONE);

/// Отметка «сейчас идёт такой-то вызов», снимается сама при выходе.
pub struct Step(&'static AtomicU8);

impl Step {
    /// Вызов с игрового потока: детур `ItemCheck` и отрисовка панели.
    pub fn game(step: u8) -> Step {
        GAME_STEP.store(step, Ordering::Relaxed);
        Step(&GAME_STEP)
    }

    /// Вызов с рабочего потока.
    pub fn worker(step: u8) -> Step {
        WORKER_STEP.store(step, Ordering::Relaxed);
        Step(&WORKER_STEP)
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        self.0.store(STEP_NONE, Ordering::Relaxed);
    }
}

pub fn install() {
    if HANDLE.load(Ordering::SeqCst) != 0 {
        return;
    }
    // Первым в очереди: до того, как исключение доберётся до чужих
    // обработчиков, которые могут увести процесс молча.
    let handle = unsafe { AddVectoredExceptionHandler(1, Some(handler)) };
    if handle.is_null() {
        crate::log!("ловушка падений не встала");
        return;
    }
    HANDLE.store(handle as usize, Ordering::SeqCst);
}

pub fn uninstall() {
    let handle = HANDLE.swap(0, Ordering::SeqCst);
    if handle != 0 {
        unsafe { RemoveVectoredExceptionHandler(handle as *mut c_void) };
    }
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    if info.is_null() {
        return CONTINUE_SEARCH;
    }
    let record = unsafe { (*info).ExceptionRecord };
    if record.is_null() {
        return CONTINUE_SEARCH;
    }
    let code = unsafe { (*record).ExceptionCode };
    let fatal = code == EXCEPTION_ACCESS_VIOLATION
        || code == EXCEPTION_STACK_OVERFLOW
        || code == EXCEPTION_ILLEGAL_INSTRUCTION;
    if !fatal {
        return CONTINUE_SEARCH;
    }
    if LOGGED.fetch_add(1, Ordering::SeqCst) >= MAX_LOGGED {
        return CONTINUE_SEARCH;
    }

    let address = unsafe { (*record).ExceptionAddress } as usize;
    // При нарушении доступа игра кладёт в параметры вид доступа и адрес,
    // по которому обратились: без них непонятно, читали или писали.
    let mut detail = String::new();
    let count = unsafe { (*record).NumberParameters } as usize;
    if code == EXCEPTION_ACCESS_VIOLATION && count >= 2 {
        let kind = unsafe { (*record).ExceptionInformation[0] };
        let target = unsafe { (*record).ExceptionInformation[1] };
        let action = match kind {
            0 => "чтение",
            1 => "запись",
            8 => "исполнение",
            _ => "доступ",
        };
        detail = format!(", {action} по 0x{target:08X}");
    }

    crate::log!(
        "ПАДЕНИЕ: код 0x{:08X} по адресу 0x{address:08X} в {}{detail} \
         | игровой поток: {}, рабочий: {}",
        code.0,
        module_of(address),
        step_name(GAME_STEP.load(Ordering::Relaxed)),
        step_name(WORKER_STEP.load(Ordering::Relaxed))
    );
    CONTINUE_SEARCH
}

/// Имя модуля, которому принадлежит адрес: по нему сразу видно, наш это
/// код, игра или драйвер.
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
            return "вне модулей (JIT или куча)".to_string();
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
