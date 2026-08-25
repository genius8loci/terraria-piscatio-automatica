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

use crate::clr::{BINDING_NON_PUBLIC, BINDING_STATIC, Clr, Field, Var, array_get};

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
    /// Сырые экранные координаты курсора: `Main.mouseX` за кадр несколько раз
    /// меняет смысл, а эти — нет. См. `cursor()`.
    raw_mouse_x: Option<Field>,
    raw_mouse_y: Option<Field>,
    mouse_left: Field,
    control_use_item: Field,
    mouse_interface: Field,
}

impl Handles {
    fn local_player(&self) -> Option<Var> {
        let index = self.my_player.get_static().ok()?.as_int()?;
        if index < 0 {
            return None;
        }
        let players = self.players.get_static().ok()?;
        let player = array_get(&players, index).ok()?;
        (!player.is_null()).then_some(player)
    }
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

    let Some(handles) = handles() else {
        FAILURES.fetch_add(1, Ordering::Relaxed);
        COMMAND.store(CMD_NONE, Ordering::Release);
        crate::log!("ввод: поднять хэндлы не удалось, команда отменена");
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
    let input = assembly.get_type("Terraria.GameInput.PlayerInput").ok();
    let raw = |name: &'static str| {
        input.as_ref().and_then(|t| {
            t.field_flags(name, BINDING_NON_PUBLIC | BINDING_STATIC)
                .ok()
        })
    };
    Some(Handles {
        my_player: main.field("myPlayer").ok()?,
        players: main.field("player").ok()?,
        mouse_x: main.field("mouseX").ok()?,
        mouse_y: main.field("mouseY").ok()?,
        raw_mouse_x: raw("_originalMouseX"),
        raw_mouse_y: raw("_originalMouseY"),
        mouse_left: main.field("mouseLeft").ok()?,
        control_use_item: player.field("controlUseItem").ok()?,
        mouse_interface: player.field("mouseInterface").ok()?,
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

/// Хэндлы для игрового потока; поднимаются лениво при первом обращении.
fn handles() -> Option<&'static Handles> {
    let slot = unsafe { &mut *HANDLES.0.get() };
    if slot.is_none() {
        *slot = attach();
        if slot.is_some() {
            crate::log!("ввод: хэндлы рефлексии подняты на игровом потоке");
        }
    }
    slot.as_ref()
}

/// Курсор и левая кнопка глазами игры, в сырых экранных пикселях.
///
/// `Main.mouseX` за кадр меняет смысл трижды: `PlayerInput.SetZoom_UI`
/// делит его на `Main.UIScale`, `SetZoom_World` пересчитывает через зум мира,
/// и только `SetZoom_Unscaled` возвращает исходное значение. Читать его,
/// не зная фазы кадра, нельзя — при масштабе интерфейса не 100% попадания
/// уезжают. Поэтому берём `PlayerInput._originalMouseX/_originalMouseY`:
/// это и есть то самое исходное значение, оно не зависит от фазы.
///
/// Звать только с игрового потока: хук Present и детур ItemCheck идут
/// по одному и тому же потоку, так что общие хэндлы безопасны.
pub fn cursor() -> Option<(i32, i32, bool)> {
    let handles = handles()?;
    let raw = |field: &Option<Field>, fallback: &Field| -> Option<i32> {
        field
            .as_ref()
            .and_then(|f| f.get_static().ok())
            .and_then(|v| v.as_int())
            .or_else(|| fallback.get_static().ok()?.as_int())
    };
    let x = raw(&handles.raw_mouse_x, &handles.mouse_x)?;
    let y = raw(&handles.raw_mouse_y, &handles.mouse_y)?;
    let down = handles
        .mouse_left
        .get_static()
        .ok()?
        .as_bool()
        .unwrap_or(false);
    Some((x, y, down))
}

/// Курсор над нашим окном — сообщаем игре, чтобы клик не ушёл в мир.
pub fn set_mouse_interface(over: bool) {
    let Some(handles) = handles() else {
        return;
    };
    let Some(player) = handles.local_player() else {
        return;
    };
    let _ = handles.mouse_interface.set(&player, Var::boolean(over));
}
