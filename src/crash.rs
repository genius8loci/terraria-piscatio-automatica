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
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

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
        "ПАДЕНИЕ: код 0x{:08X} по адресу 0x{address:08X} в {}{detail}",
        code.0,
        module_of(address)
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
