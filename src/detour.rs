//! Inline-детуры managed-методов.
//!
//! Заброс и подсечка не могут идти через реальный ввод: при свёрнутом окне
//! игра его игнорирует. Поэтому «нажатие» выставляется прямо в поле
//! `Player.controlUseItem` внутри игрового кадра — на входе в
//! `Player.ItemCheck()`, до того как метод это поле прочитает.
//!
//! Адрес JIT-кода берётся через `RuntimeMethodHandle.GetFunctionPointer()`.
//!
//! Экземплярные методы .NET на x86 передают `this` в ECX, и стабильного
//! `extern "thiscall"` в Rust нет. Поэтому детур — голая функция: сохраняет
//! все регистры, зовёт обработчик по cdecl и прыгает на трамплин, который
//! доигрывает оригинальный пролог. Так вопрос соглашения о вызове снимается
//! целиком: оригинальный код получает регистры ровно в том виде, в каком
//! они пришли.
//!
//! Второй детур — на `Main.DrawCursor`. Он нужен не ради данных, а ради
//! момента: игра зовёт его сразу после `spriteBatch.End()`, то есть весь
//! интерфейс уже выгружен на экран, а курсор ещё не нарисован. Рисуя панель
//! здесь, мы получаем её поверх интерфейса и под курсором — без второго,
//! своего курсора поверх чужого.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use minhook::MinHook;

static TRAMPOLINE: AtomicUsize = AtomicUsize::new(0);
static TARGET: AtomicUsize = AtomicUsize::new(0);
static INSTALLED: AtomicBool = AtomicBool::new(false);
/// Пока false, обработчик ничего не делает — нужно при снятии детура.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Вызывается из голого детура. `this` — указатель на `Terraria.Player`.
extern "C" fn handler(this: *mut c_void) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    // Паника внутри чужого кадра убьёт игру.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::input::on_item_check(this);
    }));
}

#[unsafe(naked)]
unsafe extern "C" fn thunk() {
    core::arch::naked_asm!(
        "pushad",
        "pushfd",
        "push ecx",
        "call {handler}",
        "add esp, 4",
        "popfd",
        "popad",
        "jmp dword ptr [{trampoline}]",
        handler = sym handler,
        trampoline = sym TRAMPOLINE,
    )
}

/// Ставит детур на JIT-код метода.
pub fn install(address: usize) -> bool {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return true;
    }
    unsafe {
        let target = address as *mut c_void;
        let original = match MinHook::create_hook(target, thunk as *const () as *mut c_void) {
            Ok(o) => o,
            Err(e) => {
                crate::log!("детур ItemCheck: create_hook не удался: {e:?}");
                INSTALLED.store(false, Ordering::SeqCst);
                return false;
            }
        };
        TRAMPOLINE.store(original as usize, Ordering::SeqCst);
        if let Err(e) = MinHook::enable_hook(target) {
            crate::log!("детур ItemCheck: enable_hook не удался: {e:?}");
            INSTALLED.store(false, Ordering::SeqCst);
            return false;
        }
        TARGET.store(address, Ordering::SeqCst);
    }
    ACTIVE.store(true, Ordering::SeqCst);
    true
}

pub fn uninstall() {
    if !INSTALLED.swap(false, Ordering::SeqCst) {
        return;
    }
    ACTIVE.store(false, Ordering::SeqCst);
    let target = TARGET.swap(0, Ordering::SeqCst);
    if target == 0 {
        return;
    }
    unsafe {
        let _ = MinHook::disable_hook(target as *mut c_void);
        let _ = MinHook::remove_hook(target as *mut c_void);
    }
    crate::log!("детур ItemCheck снят");
}

/// Стоит ли детур. Пока нет — команды ставить в очередь бессмысленно:
/// применить их некому.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Первые байты по адресу — чтобы в логе было видно, что именно патчим.
pub fn peek(address: usize) -> String {
    if address == 0 {
        return "нет адреса".to_string();
    }
    let bytes = unsafe { std::slice::from_raw_parts(address as *const u8, 16) };
    bytes.iter().map(|b| format!("{b:02X} ")).collect()
}

// ---------------------------------------------------------------------------
// Детур Main.DrawCursor
// ---------------------------------------------------------------------------

static CURSOR_TRAMPOLINE: AtomicUsize = AtomicUsize::new(0);
static CURSOR_TARGET: AtomicUsize = AtomicUsize::new(0);
static CURSOR_INSTALLED: AtomicBool = AtomicBool::new(false);
static CURSOR_ACTIVE: AtomicBool = AtomicBool::new(false);

extern "C" fn cursor_handler() {
    if !CURSOR_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::overlay::on_draw_cursor();
    }));
}

/// Аргументы `DrawCursor` нам не нужны, поэтому обработчик зовём без них:
/// регистры и стек уходят оригиналу нетронутыми.
#[unsafe(naked)]
unsafe extern "C" fn cursor_thunk() {
    core::arch::naked_asm!(
        "pushad",
        "pushfd",
        "call {handler}",
        "popfd",
        "popad",
        "jmp dword ptr [{trampoline}]",
        handler = sym cursor_handler,
        trampoline = sym CURSOR_TRAMPOLINE,
    )
}

pub fn install_cursor(address: usize) -> bool {
    if CURSOR_INSTALLED.swap(true, Ordering::SeqCst) {
        return true;
    }
    unsafe {
        let target = address as *mut c_void;
        let original = match MinHook::create_hook(target, cursor_thunk as *const () as *mut c_void)
        {
            Ok(o) => o,
            Err(e) => {
                crate::log!("детур DrawCursor: create_hook не удался: {e:?}");
                CURSOR_INSTALLED.store(false, Ordering::SeqCst);
                return false;
            }
        };
        CURSOR_TRAMPOLINE.store(original as usize, Ordering::SeqCst);
        if let Err(e) = MinHook::enable_hook(target) {
            crate::log!("детур DrawCursor: enable_hook не удался: {e:?}");
            CURSOR_INSTALLED.store(false, Ordering::SeqCst);
            return false;
        }
        CURSOR_TARGET.store(address, Ordering::SeqCst);
    }
    CURSOR_ACTIVE.store(true, Ordering::SeqCst);
    true
}

pub fn uninstall_cursor() {
    if !CURSOR_INSTALLED.swap(false, Ordering::SeqCst) {
        return;
    }
    // Гасим обработчик до снятия хука: кадр может идти прямо сейчас.
    CURSOR_ACTIVE.store(false, Ordering::SeqCst);
    let target = CURSOR_TARGET.swap(0, Ordering::SeqCst);
    if target == 0 {
        return;
    }
    unsafe {
        let _ = MinHook::disable_hook(target as *mut c_void);
        let _ = MinHook::remove_hook(target as *mut c_void);
    }
    crate::log!("детур DrawCursor снят");
}
