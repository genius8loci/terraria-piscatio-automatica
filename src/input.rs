//! Нажатие, выставляемое изнутри игрового кадра.
//!
//! Решение принимает рабочий поток, а применяет — детур `Player.ItemCheck`,
//! потому что только там гарантирован нужный момент кадра. Обмен идёт
//! атомиками: в детуре при простое не делается ни одного COM-вызова.
//!
//! Хэндлы рефлексии здесь **свои**, отдельные от тех, что у рабочего потока:
//! так каждый поток работает со своими COM-объектами и вопрос их
//! потокобезопасности не возникает.

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicU8, AtomicU32, Ordering};

use crate::clr::{Clr, Field, Var, array_get};

pub const CMD_NONE: u8 = 0;
/// Нажать в этом кадре.
pub const CMD_PRESS: u8 = 1;
/// Нажатие уже выставлено, в следующем кадре отпустить.
const CMD_RELEASE: u8 = 2;

pub static COMMAND: AtomicU8 = AtomicU8::new(CMD_NONE);
/// Экранные координаты прицела; -1 — не трогать курсор.
pub static AIM_X: AtomicI32 = AtomicI32::new(-1);
pub static AIM_Y: AtomicI32 = AtomicI32::new(-1);

/// Счётчики для лога.
pub static FIRED: AtomicU32 = AtomicU32::new(0);
pub static CLICKS: AtomicU32 = AtomicU32::new(0);
pub static FAILURES: AtomicU32 = AtomicU32::new(0);

/// Кадр, в котором команда уже применялась.
static LAST_FRAME: AtomicU32 = AtomicU32::new(u32::MAX);

struct Handles {
    _clr: Clr,
    my_player: Field,
    players: Field,
    mouse_x: Field,
    mouse_y: Field,
    control_use_item: Field,
}

/// Ячейка, к которой обращается только игровой поток.
///
/// `Drop` здесь намеренно не вызывается: деструктор в выгруженном модуле
/// уронил бы игру. При снятии детура содержимое просто утекает.
struct GameThreadCell(UnsafeCell<Option<Handles>>);
unsafe impl Sync for GameThreadCell {}

static HANDLES: GameThreadCell = GameThreadCell(UnsafeCell::new(None));

/// Вызывается из детура на входе в `Player.ItemCheck`.
pub fn on_item_check(_this: *mut c_void) {
    FIRED.fetch_add(1, Ordering::Relaxed);

    let command = COMMAND.load(Ordering::Acquire);
    if command == CMD_NONE {
        // Быстрый путь: в простое ни одного обращения к CLR.
        return;
    }

    // На сервере ItemCheck вызывается по разу на каждого игрока за кадр.
    // Без границы кадра нажатие и отпускание слиплись бы в один кадр,
    // и предмет не сработал бы. Границу даёт счётчик из хука Present;
    // если оверлей не поднят, счётчик стоит и ограничение не применяем.
    let frame = crate::FRAME.load(Ordering::Relaxed);
    if frame != 0 && LAST_FRAME.swap(frame, Ordering::Relaxed) == frame {
        return;
    }

    let handles = unsafe { &mut *HANDLES.0.get() };
    if handles.is_none() {
        match attach() {
            Some(ready) => {
                crate::log!("ввод: хэндлы рефлексии подняты на игровом потоке");
                *handles = Some(ready);
            }
            None => {
                FAILURES.fetch_add(1, Ordering::Relaxed);
                COMMAND.store(CMD_NONE, Ordering::Release);
                crate::log!("ввод: поднять хэндлы не удалось, команда отменена");
                return;
            }
        }
    }
    let Some(handles) = handles.as_ref() else {
        return;
    };

    match command {
        CMD_PRESS => {
            let aim_x = AIM_X.load(Ordering::Relaxed);
            let aim_y = AIM_Y.load(Ordering::Relaxed);
            if aim_x >= 0 && aim_y >= 0 {
                let _ = handles.mouse_x.set_static(Var::int(aim_x));
                let _ = handles.mouse_y.set_static(Var::int(aim_y));
            }
            if set_use_item(handles, true) {
                COMMAND.store(CMD_RELEASE, Ordering::Release);
            } else {
                FAILURES.fetch_add(1, Ordering::Relaxed);
                COMMAND.store(CMD_NONE, Ordering::Release);
            }
        }
        CMD_RELEASE => {
            set_use_item(handles, false);
            COMMAND.store(CMD_NONE, Ordering::Release);
            CLICKS.fetch_add(1, Ordering::Relaxed);
        }
        _ => COMMAND.store(CMD_NONE, Ordering::Release),
    }
}

fn set_use_item(handles: &Handles, pressed: bool) -> bool {
    let Ok(index) = handles
        .my_player
        .get_static()
        .map(|v| v.as_int().unwrap_or(-1))
    else {
        return false;
    };
    if index < 0 {
        return false;
    }
    let Ok(players) = handles.players.get_static() else {
        return false;
    };
    let Ok(player) = array_get(&players, index) else {
        return false;
    };
    if player.is_null() {
        return false;
    }
    handles
        .control_use_item
        .set(&player, Var::boolean(pressed))
        .is_ok()
}

fn attach() -> Option<Handles> {
    let clr = Clr::attach(false).ok()?;
    let assembly = clr.assembly("Terraria", false).ok()?;
    let main = assembly.get_type("Terraria.Main").ok()?;
    let player = assembly.get_type("Terraria.Player").ok()?;
    Some(Handles {
        my_player: main.field("myPlayer").ok()?,
        players: main.field("player").ok()?,
        mouse_x: main.field("mouseX").ok()?,
        mouse_y: main.field("mouseY").ok()?,
        control_use_item: player.field("controlUseItem").ok()?,
        _clr: clr,
    })
}

/// Поставить нажатие в очередь. `aim` — экранные координаты или `None`.
pub fn request_click(aim: Option<(i32, i32)>) {
    match aim {
        Some((x, y)) => {
            AIM_X.store(x, Ordering::Relaxed);
            AIM_Y.store(y, Ordering::Relaxed);
        }
        None => {
            AIM_X.store(-1, Ordering::Relaxed);
            AIM_Y.store(-1, Ordering::Relaxed);
        }
    }
    COMMAND.store(CMD_PRESS, Ordering::Release);
}

pub fn busy() -> bool {
    COMMAND.load(Ordering::Acquire) != CMD_NONE
}

/// Снимает зависшую команду: если детур не сработал, `busy()` иначе
/// останется истинным навсегда и автомат встанет.
pub fn cancel() {
    COMMAND.store(CMD_NONE, Ordering::Release);
    FAILURES.fetch_add(1, Ordering::Relaxed);
}
