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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use windows::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_ILLEGAL_INSTRUCTION, EXCEPTION_STACK_OVERFLOW,
};
use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_POINTERS, RemoveVectoredExceptionHandler,
    RtlCaptureStackBackTrace,
};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows::Win32::System::Threading::GetCurrentThreadId;

/// Отдать исключение следующему обработчику.
const CONTINUE_SEARCH: i32 = 0;

static HANDLE: AtomicUsize = AtomicUsize::new(0);
/// Ставим ловушку ровно один раз: `AddVectoredExceptionHandler` из двух
/// потоков дал бы два обработчика и потерянный хэндл.
static INSTALLING: AtomicBool = AtomicBool::new(false);
/// Сколько записей уже сделали: лог не должен превратиться в поток.
static LOGGED: AtomicU32 = AtomicU32::new(0);
const MAX_LOGGED: u32 = 4;
/// Поток, который прямо сейчас внутри обработчика.
///
/// Обработчик пишет в лог, а запись — это `format!`, мьютекс и `WriteFile`.
/// Если исключение прилетит изнутри самой записи, повторный вход на том же
/// потоке встанет на своём же мьютексе намертво. Поэтому вход по одному
/// потоку разрешён только один.
static HANDLING: AtomicU32 = AtomicU32::new(0);

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
pub const STEP_CURSOR: u8 = 13;
pub const STEP_AIM: u8 = 14;
pub const STEP_BITE: u8 = 15;
pub const STEP_NAMES: u8 = 16;

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
        STEP_CURSOR => "курсор и масштаб для панели",
        STEP_AIM => "точка заброса",
        STEP_BITE => "разбор поклёвки",
        STEP_NAMES => "список ловимого и имена",
        _ => "ничего",
    }
}

static GAME_STEP: AtomicU8 = AtomicU8::new(STEP_NONE);
static WORKER_STEP: AtomicU8 = AtomicU8::new(STEP_NONE);

/// Кто есть кто. Отметки шагов говорят, что мы делали, но не говорят,
/// какой поток упал: без этого «ничего, ничего» читается двусмысленно.
static GAME_THREAD: AtomicU32 = AtomicU32::new(0);
static WORKER_THREAD: AtomicU32 = AtomicU32::new(0);

pub fn mark_game_thread() {
    GAME_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
}

pub fn mark_worker_thread() {
    WORKER_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
}

/// Чей это поток, по его номеру.
fn thread_name(id: u32) -> &'static str {
    if id != 0 && id == GAME_THREAD.load(Ordering::Relaxed) {
        "игровой"
    } else if id != 0 && id == WORKER_THREAD.load(Ordering::Relaxed) {
        "рабочий"
    } else {
        "чужой"
    }
}

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
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return;
    }
    // Первым в очереди: до того, как исключение доберётся до чужих
    // обработчиков, которые могут увести процесс молча.
    let handle = unsafe { AddVectoredExceptionHandler(1, Some(handler)) };
    if handle.is_null() {
        crate::log!("ловушка падений не встала");
        INSTALLING.store(false, Ordering::SeqCst);
        return;
    }
    LOGGED.store(0, Ordering::SeqCst);
    HANDLE.store(handle as usize, Ordering::SeqCst);
}

pub fn uninstall() {
    let handle = HANDLE.swap(0, Ordering::SeqCst);
    if handle != 0 {
        unsafe { RemoveVectoredExceptionHandler(handle as *mut c_void) };
    }
    INSTALLING.store(false, Ordering::SeqCst);
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

    let faulted = unsafe { GetCurrentThreadId() };
    // Повторный вход на том же потоке — почти наверняка исключение изнутри
    // нашей же записи в лог. Второй раз туда лезть нельзя: встанем на мьютексе.
    if HANDLING.load(Ordering::SeqCst) == faulted {
        return CONTINUE_SEARCH;
    }
    if LOGGED.fetch_add(1, Ordering::SeqCst) >= MAX_LOGGED {
        return CONTINUE_SEARCH;
    }
    HANDLING.store(faulted, Ordering::SeqCst);

    // Переполнение стека: свободного стека почти не осталось, а `format!`
    // и снятие стека сами по нему и пойдут. Пишем одну готовую строку
    // и уходим — большего здесь не сделать.
    if code == EXCEPTION_STACK_OVERFLOW {
        crate::log!("ПАДЕНИЕ: переполнение стека, подробностей не будет");
        HANDLING.store(0, Ordering::SeqCst);
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
        "ПАДЕНИЕ: код 0x{:08X} в {}{detail} \
         | упал поток {} (#{faulted}) \
         | игровой занят: {}, рабочий занят: {}",
        code.0,
        frame_of(address),
        thread_name(faulted),
        step_name(GAME_STEP.load(Ordering::Relaxed)),
        step_name(WORKER_STEP.load(Ordering::Relaxed))
    );
    // Отдельной строкой: она длинная, и её удобно копировать целиком.
    let stack = backtrace();
    // Обработчик видит исключения первого шанса, а .NET раздаёт их пачками:
    // любой `NullReferenceException` в managed-коде приезжает сюда сначала
    // как нарушение доступа и обрабатывается штатно. Признак настоящей нашей
    // вины — наш модуль в стеке; без него это, скорее всего, чужая рутина.
    let ours = stack.contains("piscatio.dll");
    crate::log!(
        "ПАДЕНИЕ, стек{}: {stack}",
        if ours {
            ""
        } else {
            " (нас в нём нет — возможно, штатное исключение .NET)"
        }
    );
    HANDLING.store(0, Ordering::SeqCst);
    CONTINUE_SEARCH
}

/// Адрес в виде `модуль+смещение`: `piscatio.dll+0x1A2B`. По имени модуля
/// сразу видно, чей это код — наш, игры или рантайма.
///
/// Смещение важнее самого адреса: адреса модулей от запуска к запуску
/// разъезжаются, а смещение — нет. По нему кадр стека можно найти
/// в дизассемблере и сравнить два падения между собой.
fn frame_of(address: usize) -> String {
    match module_base(address) {
        Some((name, base)) => format!("{name}+0x{:X}", address.saturating_sub(base)),
        None => format!("0x{address:08X} (JIT или куча)"),
    }
}

/// Имя модуля и адрес его загрузки. На Windows `HMODULE` и есть база.
fn module_base(address: usize) -> Option<(String, usize)> {
    unsafe {
        let mut module = Default::default();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            windows::core::PCWSTR(address as *const u16),
            &mut module,
        )
        .ok()?;
        let mut buffer = [0u16; 260];
        let len = GetModuleFileNameW(Some(module), &mut buffer) as usize;
        if len == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buffer[..len]);
        let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
        Some((name, module.0 as usize))
    }
}

/// Стек вызовов в момент падения, кадрами вида `модуль+смещение`.
///
/// Это и есть ответ на вопрос «что именно случилось»: отметки шагов
/// говорят лишь, был ли наш вызов в работе, а стек показывает, кто кого
/// позвал. Если `piscatio.dll` в стеке есть — виноваты мы, и видно, где
/// именно; если там один `clr.dll` — рантайм упал сам по себе, и это тоже
/// ответ.
///
/// Символов у нас нет, поэтому смещение придётся смотреть в дизассемблере.
/// Зато оно стабильно между запусками, в отличие от адреса.
fn backtrace() -> String {
    const SKIP: u32 = 1;
    const DEPTH: usize = 24;
    let mut frames = [std::ptr::null_mut::<c_void>(); DEPTH];
    let captured = unsafe { RtlCaptureStackBackTrace(SKIP, &mut frames, None) } as usize;
    if captured == 0 {
        return "стек снять не удалось".to_string();
    }
    frames[..captured]
        .iter()
        .map(|frame| frame_of(*frame as usize))
        .collect::<Vec<_>>()
        .join(" <- ")
}
