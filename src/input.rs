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
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, Ordering};

use windows::core::IUnknown;

use crate::clr::{
    Assembly, BINDING_NON_PUBLIC, BINDING_STATIC, Clr, Field, Method, Type, Var, array_get,
};
use crate::crash;

pub const CMD_NONE: u8 = 0;
/// Нажать в этом кадре.
pub const CMD_PRESS: u8 = 1;
/// Нажатие уже выставлено, в следующем кадре отпустить.
const CMD_RELEASE: u8 = 2;

pub static COMMAND: AtomicU8 = AtomicU8::new(CMD_NONE);
/// Заявка «разложить по ближайшим сундукам». Отдельно от `COMMAND`:
/// раскладка не занимает кадров и с нажатием не конфликтует.
static QUICK_STACK: AtomicBool = AtomicBool::new(false);
/// Строки, которые ждут отправки в чат, и признак «есть что отправлять».
/// Признак отдельно, чтобы в простое не брать мьютекс каждый кадр.
static CHAT: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CHAT_PENDING: AtomicBool = AtomicBool::new(false);
/// Заявка «сыграть звук квестовой рыбы». Один флаг, а не счётчик: если
/// две рыбы попались подряд, второй звук поверх первого всё равно не нужен.
static SOUND: AtomicBool = AtomicBool::new(false);
/// Сколько строк держим, если игровой поток почему-то их не разбирает.
const CHAT_LIMIT: usize = 32;
/// Экранные координаты прицела; -1 — не трогать курсор.
pub static AIM_X: AtomicI32 = AtomicI32::new(-1);
pub static AIM_Y: AtomicI32 = AtomicI32::new(-1);

/// Счётчики для лога.
pub static FIRED: AtomicU32 = AtomicU32::new(0);
pub static CLICKS: AtomicU32 = AtomicU32::new(0);
pub static FAILURES: AtomicU32 = AtomicU32::new(0);
/// Сколько раз раскладка по сундукам прошла.
pub static STACKS: AtomicU32 = AtomicU32::new(0);

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
    /// `Main._uiScaleUsed` — масштаб интерфейса, выбранный игроком.
    /// Свойство `Main.UIScale` только его и возвращает, а до приватного
    /// поля дотянуться проще, чем до геттера.
    ui_scale: Option<Field>,
    /// `PlayerInput.ScrollWheelDelta` — колесо за кадр, в сотых долях.
    wheel: Option<Field>,
    control_use_item: Field,
    mouse_interface: Field,
    /// `Player.QuickStackAllChests()` — та самая кнопка из инвентаря.
    /// Не нашлась — просто не будет раскладки.
    quick_stack: Option<Method>,
    /// Строка в чат: `Main.chatMonitor` и `IChatMonitor.NewText`.
    ///
    /// Не `Main.NewText`: у неё две перегрузки, и `Type.GetMethod(String)`
    /// на таком имени бросает `AmbiguousMatchException`. У интерфейса имя
    /// одно. Заодно и тише: `Main.NewText` ещё и звук щелчка играет.
    /// Сообщение никуда не уходит — его видит только сам игрок.
    chat_monitor: Option<Field>,
    new_text: Option<Method>,
    /// Звук квестовой рыбы. Нет — просто не будет звука.
    sound: Option<SoundApi>,
    /// Всё, что нужно для подсказки предмета. Целиком необязательно:
    /// не нашлось — просто не будет подсказок.
    tooltip: Option<TooltipApi>,
    /// То же для строки поиска: не нашлось — не будет ввода.
    text: Option<TextApi>,
}

/// Руки игры, которыми играется короткий звук.
///
/// `SoundEngine.PlaySound` не годится: у неё четыре перегрузки, и
/// `Type.GetMethod(String)` на таком имени бросает `AmbiguousMatchException`.
/// А `LegacySoundPlayer.PlaySound` — одна, и `SoundEngine.LegacySoundPlayer`
/// как раз её экземпляр. Ей нужны числовые id и вариант звука, они лежат
/// в `LegacySoundStyle`, на который ссылается `SoundID.BestReforge`.
struct SoundApi {
    /// `SoundEngine.LegacySoundPlayer` и его `PlaySound`.
    player: Field,
    play: Method,
    /// `SoundID.BestReforge` и его разбор: `SoundId` — поле, `Style` — свойство.
    style: Field,
    style_id: Field,
    style_variant: Method,
}

/// Руки игры, которыми набирается текст в строке поиска.
struct TextApi {
    /// `PlayerInput.WritingText` — «клавиши сейчас про текст, не про игрока».
    /// Игра гасит его каждый кадр в `UpdateInput`, поэтому поднимать надо
    /// заново, пока строка в фокусе; забыть — само отпустит.
    writing: Field,
    /// `Main.instance` и его `HandleIME()`: он наполняет буфер символов,
    /// из которого `GetInputText` и берёт набранное.
    instance: Field,
    handle_ime: Method,
    /// `Main.GetInputText(string)` — весь разбор клавиш уже внутри.
    get_input: Method,
}

/// Руки игры, которыми показывается подсказка предмета.
struct TooltipApi {
    /// `Main.HoverItem` — предмет, о котором рассказывает подсказка.
    hover_item: Field,
    /// `Main.DisplayAndGetFakeItem` — заводит очередь подсказки; её рисует
    /// `DrawPendingMouseText` в самом конце интерфейса.
    display: Method,
    /// `Item.Clone` — чтобы завести свой экземпляр, не трогая чужие.
    clone: Method,
    /// `Item.netDefaults` — единственная неперегруженная настройка по id.
    net_defaults: Method,
    rare: Field,
    /// `Main.instance` и `Main.MouseTextNoOverride` — ими игра показывает
    /// подсказку простым текстом, без предмета. Ровно так подписана кнопка
    /// «разложить по сундукам» в инвентаре. Необязательны: не нашлись —
    /// пропадёт только текстовая подсказка, подсказки предметов останутся.
    instance: Field,
    mouse_text: Option<Method>,
}

/// Предмет, о котором сейчас рассказывает подсказка.
struct Hovered {
    item: IUnknown,
    id: i32,
    rare: i32,
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
struct GameThreadCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for GameThreadCell<T> {}

static HANDLES: GameThreadCell<Option<Handles>> = GameThreadCell(UnsafeCell::new(None));
static HOVERED: GameThreadCell<Option<Hovered>> = GameThreadCell(UnsafeCell::new(None));

/// Вызывается из детура на входе в `Player.ItemCheck`.
pub fn on_item_check(_this: *mut c_void) {
    FIRED.fetch_add(1, Ordering::Relaxed);

    let command = COMMAND.load(Ordering::Acquire);
    let stack = QUICK_STACK.load(Ordering::Acquire);
    let chat = CHAT_PENDING.load(Ordering::Acquire);
    let sound = SOUND.load(Ordering::Acquire);
    if command == CMD_NONE && !stack && !chat && !sound {
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
        QUICK_STACK.store(false, Ordering::Release);
        CHAT_PENDING.store(false, Ordering::Release);
        if let Ok(mut queue) = CHAT.lock() {
            queue.clear();
        }
        crate::log!("ввод: поднять хэндлы не удалось, команда отменена");
        return;
    };

    if stack {
        QUICK_STACK.store(false, Ordering::Release);
        let _step = crash::Step::game(crash::STEP_QUICK_STACK);
        quick_stack(handles);
    }

    if chat {
        let _step = crash::Step::game(crash::STEP_CHAT);
        flush_chat(handles);
    }

    if sound {
        SOUND.store(false, Ordering::Release);
        let _step = crash::Step::game(crash::STEP_SOUND);
        play_sound(handles);
    }

    let _step = crash::Step::game(crash::STEP_CLICK);
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

/// Разложить по ближайшим сундукам руками самой игры.
///
/// Звать можно только отсюда, с игрового потока. `QuickStacking` ведёт весь
/// разбор в общих статических буферах — `NearbyChests._scratch`,
/// `inventoryItemsScratch`, пул `DestinationHelper` — и переставляет предметы
/// прямо в `player.inventory` и в `Chest.item`. Ни одного замка там нет:
/// сама игра зовёт это из `Main.DrawInventory`, то есть строго из своего
/// кадра. С рабочего потока вызов читал наполовину собранное состояние и
/// падал по нулевому указателю в JIT-коде (0xC0000005 в логе), а рефлексия
/// показывала это как «Адресат вызова создал исключение».
fn quick_stack(handles: &Handles) {
    let Some(method) = handles.quick_stack.as_ref() else {
        crate::log!("сундуки: метода QuickStackAllChests нет, раскладка недоступна");
        return;
    };
    let Some(player) = handles.local_player() else {
        return;
    };
    match method.invoke(&player, &[]) {
        Ok(_) => {
            STACKS.fetch_add(1, Ordering::Relaxed);
            crate::log!("инвентарь полон — разложил по ближайшим сундукам");
        }
        Err(e) => crate::log!("разложить по сундукам не удалось: {e}"),
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
        ui_scale: main
            .field_flags("_uiScaleUsed", BINDING_NON_PUBLIC | BINDING_STATIC)
            .ok(),
        wheel: input
            .as_ref()
            .and_then(|t| t.field("ScrollWheelDelta").ok()),
        control_use_item: player.field("controlUseItem").ok()?,
        mouse_interface: player.field("mouseInterface").ok()?,
        quick_stack: player.method("QuickStackAllChests").ok(),
        chat_monitor: main.field("chatMonitor").ok(),
        new_text: assembly
            .get_type("Terraria.GameContent.UI.Chat.IChatMonitor")
            .ok()
            .and_then(|t| t.method("NewText").ok()),
        sound: sound_api(&assembly),
        tooltip: tooltip_api(&assembly, &main),
        text: text_api(&main, input.as_ref()),
        _clr: clr,
    })
}

/// Собирает всё нужное для короткого звука. Не нашлось — просто не будет
/// звука, остальное работает.
fn sound_api(assembly: &Assembly) -> Option<SoundApi> {
    let engine = assembly.get_type("Terraria.Audio.SoundEngine").ok()?;
    let legacy = assembly.get_type("Terraria.Audio.LegacySoundPlayer").ok()?;
    let style_type = assembly.get_type("Terraria.Audio.LegacySoundStyle").ok()?;
    let sound_id = assembly.get_type("Terraria.ID.SoundID").ok()?;
    Some(SoundApi {
        player: engine.field("LegacySoundPlayer").ok()?,
        play: legacy.method("PlaySound").ok()?,
        style: sound_id.field("BestReforge").ok()?,
        style_id: style_type.field("SoundId").ok()?,
        style_variant: style_type.method("get_Style").ok()?,
    })
}

fn text_api(main: &Type, input: Option<&Type>) -> Option<TextApi> {
    Some(TextApi {
        writing: input?.field("WritingText").ok()?,
        instance: main.field("instance").ok()?,
        handle_ime: main.method("HandleIME").ok()?,
        get_input: main.method("GetInputText").ok()?,
    })
}

/// Собирает всё нужное для подсказки. Ни одно из этого не критично:
/// не нашлось — просто не будет подсказок, остальное работает.
fn tooltip_api(assembly: &Assembly, main: &Type) -> Option<TooltipApi> {
    let item = assembly.get_type("Terraria.Item").ok()?;
    Some(TooltipApi {
        hover_item: main.field("HoverItem").ok()?,
        display: main.method("DisplayAndGetFakeItem").ok()?,
        clone: item.method("Clone").ok()?,
        net_defaults: item.method("netDefaults").ok()?,
        rare: item.field("rare").ok()?,
        instance: main.field("instance").ok()?,
        mouse_text: main.method("MouseTextNoOverride").ok(),
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

/// Поставить в очередь раскладку по ближайшим сундукам. Исполнит её
/// игровой поток в ближайшем кадре, см. `quick_stack`.
pub fn request_quick_stack() {
    QUICK_STACK.store(true, Ordering::Release);
}

/// Заявка ещё не исполнена.
pub fn quick_stack_pending() -> bool {
    QUICK_STACK.load(Ordering::Acquire)
}

/// Поставить строку в очередь на отправку в чат. Отправит её игровой поток
/// в ближайшем кадре, см. `flush_chat`.
pub fn queue_chat(line: String) {
    let Ok(mut queue) = CHAT.lock() else {
        return;
    };
    // Если разбирать очередь некому, старые строки уже неинтересны.
    if queue.len() >= CHAT_LIMIT {
        queue.remove(0);
    }
    queue.push(line);
    CHAT_PENDING.store(true, Ordering::Release);
}

/// Поставить в очередь звук квестовой рыбы. Сыграет его игровой поток
/// в ближайшем кадре, см. `play_sound`.
pub fn request_sound() {
    SOUND.store(true, Ordering::Release);
}

/// Играет короткий звук руками игры — чтобы он слушался её громкости
/// и глушился вместе с остальным звуком. Только с игрового потока.
fn play_sound(handles: &Handles) {
    let Some(api) = handles.sound.as_ref() else {
        return;
    };
    let (Ok(style), Ok(player)) = (api.style.get_static(), api.player.get_static()) else {
        return;
    };
    if style.is_null() || player.is_null() {
        return;
    }
    let Some(id) = api.style_id.get(&style).ok().and_then(|v| v.as_int()) else {
        return;
    };
    let variant = api
        .style_variant
        .invoke(&style, &[])
        .ok()
        .and_then(|v| v.as_int())
        .unwrap_or(1);
    // Координаты -1 означают «без привязки к месту»: звук звучит ровно так,
    // как интерфейсный, а не из точки в мире.
    let _ = api.play.invoke(
        &player,
        &[
            Var::int(id),
            Var::int(-1),
            Var::int(-1),
            Var::int(variant),
            Var::float(1.0),
            Var::float(0.0),
        ],
    );
}

/// Отдаёт накопленные строки чату игры. Только с игрового потока: чат —
/// её собственный список, и правит его она сама в своём кадре.
fn flush_chat(handles: &Handles) {
    let lines: Vec<String> = match CHAT.lock() {
        Ok(mut queue) => std::mem::take(&mut *queue),
        Err(_) => return,
    };
    CHAT_PENDING.store(false, Ordering::Release);
    let (Some(monitor_field), Some(new_text)) =
        (handles.chat_monitor.as_ref(), handles.new_text.as_ref())
    else {
        return;
    };
    let Ok(monitor) = monitor_field.get_static() else {
        return;
    };
    if monitor.is_null() {
        return;
    }
    for line in lines {
        // Цвет задаётся тегами прямо в строке, поэтому сюда отдаём белый.
        let _ = new_text.invoke(
            &monitor,
            &[
                Var::text(&line),
                Var::byte(255),
                Var::byte(255),
                Var::byte(255),
            ],
        );
    }
}

/// Снять зависшую заявку: без детура исполнять её некому, а автомат
/// иначе будет ждать её вечно.
pub fn cancel_quick_stack() {
    QUICK_STACK.store(false, Ordering::Release);
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
            // Заодно запоминаем, какой поток тут игровой: ловушке падений
            // это нужно, чтобы сказать, кто именно упал.
            crash::mark_game_thread();
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
    let _step = crash::Step::game(crash::STEP_CURSOR);
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

/// Масштаб интерфейса, выставленный игроком в настройках. Ровно на столько
/// игра увеличивает свой UI, и наша панель должна расти вместе с ним.
pub fn ui_scale() -> Option<f32> {
    let _step = crash::Step::game(crash::STEP_CURSOR);
    handles()?.ui_scale.as_ref()?.get_static().ok()?.as_float()
}

/// Колесо мыши за этот кадр, в «щелчках»: игра держит его в сотых долях,
/// один щелчок — 120.
pub fn wheel() -> i32 {
    let _step = crash::Step::game(crash::STEP_CURSOR);
    let Some(handles) = handles() else {
        return 0;
    };
    let Some(field) = handles.wheel.as_ref() else {
        return 0;
    };
    let raw = field
        .get_static()
        .ok()
        .and_then(|v| v.as_int())
        .unwrap_or(0);
    raw / 120
}

/// Курсор над нашим окном — сообщаем игре, чтобы клик не ушёл в мир.
///
/// Только выставляем флаг, никогда не снимаем. `Player.mouseInterface` —
/// общий на всех: игра гасит его один раз за кадр в `Main.DoUpdate`, а потом
/// каждый, кто держит под курсором свою кнопку, поднимает заново. Записать
/// туда `false` — значит стереть чужое «да», и клик по кнопке торговца
/// уходит в мир вместо магазина.
/// Показывает подсказку игры для предмета — ту самую, что в инвентаре.
///
/// Ничего не рисуем сами: `Main.DisplayAndGetFakeItem` наполняет очередь
/// подсказки, а рисует её `DrawPendingMouseText` в самом конце интерфейса,
/// уже после нас. Текст берётся из `Main.HoverItem`, туда и кладём свой
/// экземпляр `Item` — заведённый один раз клоном и настроенный по id.
/// Звать только с игрового потока, изнутри отрисовки интерфейса.
pub fn show_item_tooltip(id: i32) {
    let _step = crash::Step::game(crash::STEP_ITEM_TOOLTIP);
    let Some(handles) = handles() else {
        return;
    };
    let Some(api) = handles.tooltip.as_ref() else {
        return;
    };

    let slot = unsafe { &mut *HOVERED.0.get() };
    if slot.as_ref().map(|h| h.id) != Some(id) {
        *slot = make_hovered(api, id);
    }
    let Some(hovered) = slot.as_ref() else {
        return;
    };

    // Редкость отдаём игре: от неё зависит цвет имени в подсказке.
    if api
        .display
        .invoke(&Var::null(), &[Var::int(hovered.rare)])
        .is_err()
    {
        return;
    }
    let _ = api.hover_item.set_static(Var::object(&hovered.item));
}

/// Показывает подсказку простым текстом — ту же, что у кнопок инвентаря.
///
/// `Main.MouseTextNoOverride(string, int, byte, int, int, int, int, int)`:
/// все семь хвостовых параметров со значениями по умолчанию, но рефлексия
/// их не подставляет, поэтому передаём ровно то, что подставил бы компилятор.
/// Звать только с игрового потока, изнутри отрисовки интерфейса.
pub fn show_text_tooltip(text: &str) {
    let _step = crash::Step::game(crash::STEP_TEXT_TOOLTIP);
    let Some(handles) = handles() else {
        return;
    };
    let Some(api) = handles.tooltip.as_ref() else {
        return;
    };
    let Some(mouse_text) = api.mouse_text.as_ref() else {
        return;
    };
    let Ok(instance) = api.instance.get_static() else {
        return;
    };
    if instance.is_null() {
        return;
    }
    let _ = mouse_text.invoke(
        &instance,
        &[
            Var::text(text),
            Var::int(0),
            Var::byte(0),
            Var::int(-1),
            Var::int(-1),
            Var::int(-1),
            Var::int(-1),
            Var::int(0),
        ],
    );
}

/// Отдаёт строку поиска игре на правку: она сама разберёт нажатия,
/// Backspace, Ctrl+V и раскладку. Возвращает новое значение.
///
/// Звать каждый кадр, пока строка в фокусе, и только оттуда же, откуда это
/// делает сама игра — из отрисовки интерфейса. `WritingText` при этом
/// поднимается заново: игра гасит его каждый кадр, так что стоит перестать
/// звать — и клавиши сразу вернутся игроку.
pub fn edit_text(current: &str) -> Option<String> {
    let _step = crash::Step::game(crash::STEP_SEARCH_TEXT);
    let handles = handles()?;
    let api = handles.text.as_ref()?;

    api.writing.set_static(Var::boolean(true)).ok()?;
    let instance = api.instance.get_static().ok()?;
    if !instance.is_null() {
        let _ = api.handle_ime.invoke(&instance, &[]);
    }
    // Второй аргумент обязателен, хотя у метода он со значением по умолчанию:
    // рефлексия значения по умолчанию не подставляет и на нехватке аргументов
    // бросает `TargetParameterCountException`.
    api.get_input
        .invoke(&Var::null(), &[Var::text(current), Var::boolean(false)])
        .ok()?
        .as_string()
}

/// Заводит свой экземпляр `Item` под нужный id.
fn make_hovered(api: &TooltipApi, id: i32) -> Option<Hovered> {
    // Клонируем то, что лежит в `HoverItem`: это всегда живой `Item`,
    // и `Clone` — единственный неперегруженный способ получить свой.
    let base = api.hover_item.get_static().ok()?;
    let item = api.clone.invoke(&base, &[]).ok()?;
    api.net_defaults.invoke(&item, &[Var::int(id)]).ok()?;
    let rare = api.rare.get(&item).ok()?.as_int().unwrap_or(0);
    Some(Hovered {
        item: item.as_unknown()?,
        id,
        rare,
    })
}

pub fn claim_mouse_interface() {
    let _step = crash::Step::game(crash::STEP_CURSOR);
    let Some(handles) = handles() else {
        return;
    };
    let Some(player) = handles.local_player() else {
        return;
    };
    let _ = handles.mouse_interface.set(&player, Var::boolean(true));
}
