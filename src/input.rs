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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::core::IUnknown;

use crate::clr::{
    Assembly, BINDING_NON_PUBLIC, BINDING_STATIC, Clr, Field, Method, Type, Var, array_get,
};
use crate::crash;

pub const CMD_NONE: u8 = 0;
/// Нажать в этом тике.
pub const CMD_PRESS: u8 = 1;
/// Нажатие уже выставлено, в следующем тике отпустить.
const CMD_RELEASE: u8 = 2;

pub static COMMAND: AtomicU8 = AtomicU8::new(CMD_NONE);
/// Нажатие сняли на полпути: оно уже выставлено, а отпустить некому.
/// Поле надо вернуть в исходное состояние, иначе мы оставим за собой
/// «кнопка держится» — игра его перетирает каждый тик, но полагаться
/// на это значит зависеть от порядка внутри чужого кадра.
static RELEASE: AtomicBool = AtomicBool::new(false);
/// Заявка «разложить по ближайшим сундукам». Отдельно от `COMMAND`:
/// раскладка не занимает кадров и с нажатием не конфликтует.
static QUICK_STACK: AtomicBool = AtomicBool::new(false);
/// Строки, которые ждут отправки в чат, и признак «есть что отправлять».
/// Признак отдельно, чтобы в простое не брать мьютекс каждый кадр.
static CHAT: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CHAT_PENDING: AtomicBool = AtomicBool::new(false);
/// Заявки на звуки, битами `SOUND_*`. Не счётчик: если два одинаковых
/// повода случились подряд, второй звук поверх первого всё равно не нужен.
static SOUND: AtomicU8 = AtomicU8::new(0);

/// Квестовая рыба — тот же звук, что у лучшей перековки (`SoundID.BestReforge`).
/// Это `LegacySoundStyle`, поэтому id и вариант из него ещё надо достать.
pub const SOUND_QUEST: u8 = 1;
/// Наведение и нажатие на кнопку панели: `SoundID.MenuTick` (12).
pub const SOUND_TICK: u8 = 2;
/// Служебное сообщение автомата в чат: `SoundID.Chat` (24).
pub const SOUND_CHAT: u8 = 4;
/// Глоток автопитья: `SoundID.Item3`, тот же звук, что у зелий и еды в руках
/// игрока (`Item.UseSound` у всех пяти наших предметов именно он).
pub const SOUND_DRINK: u8 = 8;
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

/// Тик, в котором команда уже применялась.
static LAST_TICK: AtomicU32 = AtomicU32::new(u32::MAX);
/// Подсказка предмета уже срывалась — второй раз в лог не пишем.
static TOOLTIP_FAILED: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Граница игрового тика
// ---------------------------------------------------------------------------
//
// Считается по указателю `this`, без единого обращения к CLR.
//
// `Player.ItemCheck` вызывается ровно раз на игрока за тик — замерено:
// шестьдесят вызовов в секунду при шестидесяти тиках. Порядок игроков внутри
// тика постоянен, значит новый тик начинается всякий раз, когда снова приходит
// тот же `this`, с которого тик начался. В одиночной игре игрок один, и каждый
// вызов — новый тик.
//
// Спрашивать номер у самой игры (`Main.GameUpdateCount`) было **нельзя**, и это
// стоило падения. Вызов рефлексии — это работа в управляемой куче, и делался он
// шестьдесят раз в секунду прямо из пролога чужого managed-метода, где кадр
// стека ещё не построен: наш детур стоит на первых байтах `ItemCheck`. Сборка
// мусора, случившаяся в этот момент, идёт разбирать стек потока и упирается
// в этот полукадр. В логе это выглядело как `0xC0000005`, чтение по нулю,
// на игровом потоке, при пустых отметках занятости — потому что отметки
// у того вызова не было вовсе.
//
// Отсюда правило: **на холостом ходу детур не трогает CLR ни разу.**

/// Номер тика. Растёт только на игровом потоке.
static TICK: AtomicU32 = AtomicU32::new(0);
/// `this` игрока, с которого начинается тик.
static TICK_OWNER: AtomicUsize = AtomicUsize::new(0);
/// Сколько вызовов подряд пришло мимо хозяина тика.
static TICK_MISSES: AtomicU32 = AtomicU32::new(0);
/// Столько промахов подряд — и хозяином становится текущий игрок. Нужно, если
/// прежний вышел из игры или сборщик мусора переместил объект: без этого
/// граница тика замерла бы навсегда.
const TICK_REARM: u32 = 64;

/// Номер текущего игрового тика по указателю игрока.
fn tick_of(this: *mut c_void) -> u32 {
    let this = this as usize;
    let owner = TICK_OWNER.load(Ordering::Relaxed);
    if owner == this {
        TICK_MISSES.store(0, Ordering::Relaxed);
        return TICK.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    }
    if owner == 0 || TICK_MISSES.fetch_add(1, Ordering::Relaxed) >= TICK_REARM {
        TICK_OWNER.store(this, Ordering::Relaxed);
        TICK_MISSES.store(0, Ordering::Relaxed);
        return TICK.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    }
    // Тот же тик, просто другой игрок.
    TICK.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Выдержка тиков при свёрнутом окне
// ---------------------------------------------------------------------------
//
// Это **страховка**, а не основной механизм. В норме свёрнутую игру держит
// она сама: `Main.DoUpdate` выставляет
// `IsFixedTimeStep = ThrottleWhenInactive && !IsActive`, то есть при потере
// фокуса переходит на фиксированный шаг времени, и мир идёт ровно 60 тиков
// в секунду. Прежние версии piscatio этот переход отключали (снимали
// `ThrottleWhenInactive`, считая его просто сном) — и цикл разгонялся
// до тысяч тиков в секунду: замерено 2408 против 60, сутки за полминуты.
// Теперь настройку игры мы не трогаем вовсе, см. комментарий в `app`.
//
// Но у игрока в `config.json` игры может остаться `ThrottleWhenInactive:
// false` — хоть от старых наших версий, хоть выставленный вручную. Тогда
// ограничителя у свёрнутой игры не будет, и держим её мы. `Player.ItemCheck`
// вызывается ровно раз на игрока за тик (проверено по декомпиляции: ровно
// один вызов из `Player.Update`), так что это единственная точка, идущая
// в ногу с циклом и при свёрнутом окне тоже.
//
// Когда игра держит себя сама, сюда мы приходим с уже истёкшим сроком
// и не спим ни разу — то есть в обычном случае эта ветка бесплатна.

/// Сколько времени отводится одному тику: 60 в секунду, как у игры.
const TICK_PERIOD: Duration = Duration::from_micros(16_667);
/// Насколько игре позволено забегать вперёд, не встречая выдержки.
///
/// Догоняющий цикл XNA после сна при потере фокуса выдаёт по два-три
/// `Update` подряд — это штатная работа, и мешать ей нельзя. Четверть
/// секунды запаса покрывает такие пачки с избытком, а настоящий разгон
/// (тысячи тиков в секунду) съедает её за один кадр.
const TICK_BURST: Duration = Duration::from_millis(250);

/// Окно игры не впереди — выдержку держим мы. Ставит рабочий поток.
static INACTIVE: AtomicBool = AtomicBool::new(false);
/// Разрешение системного таймера поднято до миллисекунды.
static PERIOD_RAISED: AtomicBool = AtomicBool::new(false);

/// Сообщает, впереди ли окно игры. Зовёт рабочий поток, раз в опрос.
pub fn set_window_active(active: bool) {
    INACTIVE.store(!active, Ordering::Relaxed);
}

/// Впереди ли окно игры — нужно строке падения: свёрнутое окно живёт иначе.
pub fn window_active() -> bool {
    !INACTIVE.load(Ordering::Relaxed)
}

/// Номер тика и время, на которое он был выдержан.
static PACE: GameThreadCell<Option<(u32, Instant)>> = GameThreadCell(UnsafeCell::new(None));

/// Ограничивает **среднюю** скорость неактивной игры шестьюдесятью тиками
/// в секунду, не мешая всплескам.
///
/// Всплески трогать нельзя, и это выяснилось дорого. При `IsFixedTimeStep`
/// XNA после сна догоняет накопленное время: прогоняет два-три `Update`
/// подряд в одном проходе цикла. Прежняя выдержка усыпляла **каждый**
/// `Update` на 16.7 мс и встревала прямо внутрь этого догоняющего цикла —
/// проход растягивался, накапливалось ещё больше времени, следующий проход
/// требовал ещё больше обновлений. Частота кадров складывалась до 5–10.
///
/// Поэтому здесь не «сон между тиками», а разрешение бежать вперёд:
/// накопленному опережению позволяем дорасти до `TICK_BURST`, и только
/// сверх него досыпаем. Ровная работа игры (шестьдесят тиков в секунду,
/// хоть бы и пачками) опережения не создаёт вовсе, и сна не происходит
/// ни разу. Разгон же создаёт его мгновенно.
///
/// Звать только при неактивном окне — отметку сбрасывает вызывающий, как
/// только окно возвращается. Ни одного обращения к CLR: номер тика приходит
/// готовым, см. `tick_of`.
fn pace(tick: u32) {
    // Без миллисекундного разрешения `Sleep` округляется до 15.6 мс, и вместо
    // шестидесяти тиков вышло бы тридцать два.
    if !PERIOD_RAISED.swap(true, Ordering::Relaxed) {
        let _ = unsafe { timeBeginPeriod(1) };
    }

    let slot = unsafe { &mut *PACE.0.get() };
    let now = Instant::now();
    let Some((last, credit)) = *slot else {
        *slot = Some((tick, now));
        return;
    };
    // Тот же тик, просто другой игрок: в сетевой игре `ItemCheck` вызывается
    // по разу на каждого. Второй раз этот тик не считаем.
    if last == tick {
        return;
    }

    // `credit` — мгновение, до которого игра уже «отработала» тиками.
    // Обогнала реальное время больше чем на `TICK_BURST` — придерживаем.
    if let Some(ahead) = credit.checked_duration_since(now)
        && ahead > TICK_BURST
    {
        std::thread::sleep(ahead - TICK_BURST);
    }
    // Отстала — начинаем счёт заново от текущего мгновения, иначе долг
    // копился бы и потом выстрелил пачкой пропущенных выдержек.
    let base = credit.max(now);
    *slot = Some((tick, base + TICK_PERIOD));
}

/// Окно вернулось: выдержка не нужна.
///
/// Отметку прошлого тика сбрасываем, иначе первый же свёрнутый тик сравнится
/// с давним временем и выдержки не будет. И возвращаем системе разрешение
/// таймера: миллисекундное имеет смысл, только пока мы сами отмеряем сон,
/// а держать его всю сессию — зря греть батарею на ноутбуке.
fn pace_idle() {
    unsafe { *PACE.0.get() = None };
    if PERIOD_RAISED.swap(false, Ordering::Relaxed) {
        let _ = unsafe { timeEndPeriod(1) };
    }
}

/// Возвращает системе прежнее разрешение таймера и гасит выдержку: рабочего
/// потока больше нет, обновлять признак активности окна некому.
pub fn shutdown() {
    INACTIVE.store(false, Ordering::Relaxed);
    if PERIOD_RAISED.swap(false, Ordering::SeqCst) {
        let _ = unsafe { timeEndPeriod(1) };
    }
}

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
pub fn on_item_check(this: *mut c_void) {
    FIRED.fetch_add(1, Ordering::Relaxed);

    // Номер тика — по указателю игрока, без обращений к CLR: см. `tick_of`.
    let tick = tick_of(this);

    // Выдержку держим всегда, а не только когда есть что делать: без неё
    // свёрнутая игра разгоняет мир, даже если автомат простаивает.
    if INACTIVE.load(Ordering::Relaxed) {
        pace(tick);
    } else {
        pace_idle();
    }

    let command = COMMAND.load(Ordering::Acquire);
    let stack = QUICK_STACK.load(Ordering::Acquire);
    let release = RELEASE.load(Ordering::Acquire);
    if command == CMD_NONE && !stack && !release {
        // Быстрый путь: у игры впереди — ни одного обращения к CLR.
        return;
    }

    let Some(handles) = handles() else {
        FAILURES.fetch_add(1, Ordering::Relaxed);
        COMMAND.store(CMD_NONE, Ordering::Release);
        RELEASE.store(false, Ordering::Release);
        QUICK_STACK.store(false, Ordering::Release);
        crate::log!("ввод: поднять хэндлы не удалось, команда отменена");
        return;
    };

    // В сетевой игре `ItemCheck` вызывается по разу на каждого игрока за тик.
    // Без границы тика нажатие и отпускание слиплись бы в один, и предмет
    // не сработал бы.
    if LAST_TICK.swap(tick, Ordering::Relaxed) == tick {
        return;
    }

    if release {
        RELEASE.store(false, Ordering::Release);
        let _step = crash::Step::game(crash::STEP_CLICK);
        set_use_item_on(handles, this, false);
    }

    if stack {
        QUICK_STACK.store(false, Ordering::Release);
        let _step = crash::Step::game(crash::STEP_QUICK_STACK);
        quick_stack(handles);
    }

    let _step = crash::Step::game(crash::STEP_CLICK);
    match command {
        CMD_PRESS => {
            // Смещение поля ищем на первом же нажатии: заброс и так уже
            // требует обращений к CLR, так что лишнего риска здесь нет,
            // а все следующие нажатия пойдут уже без них.
            probe_use_item(handles, this);
            let aim_x = AIM_X.load(Ordering::Relaxed);
            let aim_y = AIM_Y.load(Ordering::Relaxed);
            if aim_x >= 0 && aim_y >= 0 {
                let _ = handles.mouse_x.set_static(Var::int(aim_x));
                let _ = handles.mouse_y.set_static(Var::int(aim_y));
            }
            if set_use_item_on(handles, this, true) {
                COMMAND.store(CMD_RELEASE, Ordering::Release);
            } else {
                FAILURES.fetch_add(1, Ordering::Relaxed);
                COMMAND.store(CMD_NONE, Ordering::Release);
            }
        }
        CMD_RELEASE => {
            set_use_item_on(handles, this, false);
            COMMAND.store(CMD_NONE, Ordering::Release);
            CLICKS.fetch_add(1, Ordering::Relaxed);
        }
        _ => COMMAND.store(CMD_NONE, Ordering::Release),
    }
}

/// Отдаёт игре накопленные строки чата и звук квестовой рыбы.
///
/// Зовётся из хука `Present`, а не из детура `ItemCheck`, и это важно.
/// В `Present` игра приходит через P/Invoke: поток в вытесняющем режиме
/// и с честной переходной рамкой, так что сборка мусора разбирает такой
/// стек нормально. Детур же стоит на первых байтах managed-метода, где
/// кадр ещё не построен, — а `MethodInfo.Invoke` сам выделяет память
/// и потому может запустить сборку ровно в этот момент. Именно так игра
/// и умирала (`mark_object_simple`, чтение по нулю).
///
/// Плата — задержка до кадра: чат и звук отстают от события на 16 мс.
/// На глаз это незаметно, а риска больше нет.
///
/// Раскладка по сундукам сюда **не** перенесена: игра зовёт её из
/// `Main.DrawInventory`, то есть из середины кадра, и что она делает
/// с общими буферами после отрисовки — не проверено. Идёт единицами
/// в минуту, так что осталась в детуре.
pub fn on_present() {
    if !CHAT_PENDING.load(Ordering::Acquire) && SOUND.load(Ordering::Acquire) == 0 {
        return;
    }
    let Some(handles) = handles() else {
        CHAT_PENDING.store(false, Ordering::Release);
        SOUND.store(0, Ordering::Release);
        chat_queue().clear();
        return;
    };
    if CHAT_PENDING.load(Ordering::Acquire) {
        let _step = crash::Step::game(crash::STEP_CHAT);
        flush_chat(handles);
    }
    let wanted = SOUND.swap(0, Ordering::AcqRel);
    if wanted != 0 {
        let _step = crash::Step::game(crash::STEP_SOUND);
        play_sound(handles, wanted);
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

// ---------------------------------------------------------------------------
// Нажатие без обращения к CLR
// ---------------------------------------------------------------------------
//
// Ради чего всё это. Выяснено по расшифрованному дампу падения: сборка мусора
// пошла разбирать стек игрового потока (`Thread::StackWalkFrames` ->
// `EECodeManager::EnumGcRefs`), приняла мусор за ссылку на объект и умерла
// в `mark_object_simple`. Причина — наш же детур: он стоит на первых байтах
// `Player.ItemCheck`, а голая заглушка кладёт на стек десять двойных слов.
// Сборщик определяет метод по адресу возврата, декодирует его GC-информацию
// для смещения ноль и ищет ссылки относительно `ESP` — а тот съехал.
//
// Само по себе это опасно лишь в момент сборки мусора. Но `MethodInfo.Invoke`
// и `FieldInfo.SetValue` **сами выделяют память**, то есть каждый вызов
// рефлексии отсюда мог запустить сборку ровно тогда, когда наш кривой кадр
// лежит на стеке. Это не гонка, а рулетка.
//
// Поэтому нажатие ставится записью одного байта прямо в объект игрока.
// Смещение поля не зашито: оно вычисляется один раз на живой игре и тут же
// проверяется через ту же рефлексию. Не сошлось — остаёмся на рефлексии.

/// Смещение `Player.controlUseItem` внутри объекта; 0 — ещё не знаем.
static USE_ITEM_OFFSET: AtomicUsize = AtomicUsize::new(0);
/// Вычислять смещение пробуем ровно один раз за сессию.
static USE_ITEM_PROBED: AtomicBool = AtomicBool::new(false);
/// Одиночная игра: только в ней `this` в детуре — заведомо наш игрок.
/// В сетевой `ItemCheck` зовётся за каждого, и писать в чужого нельзя.
static SINGLE_PLAYER: AtomicBool = AtomicBool::new(false);
/// Дальше этого от начала объекта не смотрим. `Terraria.Player` большой,
/// но не настолько.
const PROBE_LIMIT: usize = 4096;

/// Сообщает, одиночная ли игра. Зовёт рабочий поток по `Main.netMode`.
pub fn set_single_player(single: bool) {
    SINGLE_PLAYER.store(single, Ordering::Relaxed);
}

/// Ставит нажатие. Быстрый путь — запись байта, запасной — рефлексия.
fn set_use_item_on(handles: &Handles, this: *mut c_void, pressed: bool) -> bool {
    let offset = USE_ITEM_OFFSET.load(Ordering::Relaxed);
    if offset != 0 && !this.is_null() && SINGLE_PLAYER.load(Ordering::Relaxed) {
        unsafe { this.cast::<u8>().add(offset).write(u8::from(pressed)) };
        return true;
    }
    set_use_item(handles, pressed)
}

/// Сколько байт объекта можно читать, не рискуя выйти за отображённую память.
fn readable_span(this: *mut c_void) -> usize {
    use windows::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
    let mut info = MEMORY_BASIC_INFORMATION::default();
    let size = size_of::<MEMORY_BASIC_INFORMATION>();
    if unsafe { VirtualQuery(Some(this), &mut info, size) } == 0 {
        return 0;
    }
    let end = info.BaseAddress as usize + info.RegionSize;
    let room = end.saturating_sub(this as usize);
    room.min(PROBE_LIMIT)
}

/// Ищет смещение `controlUseItem` и проверяет находку.
///
/// Всё внутри одного вызова детура: игровой поток стоит здесь же, поэтому
/// между слепками объект никто не трогает и меняется ровно наше поле.
/// Если сборка мусора успеет переместить объект, слепки окажутся о разной
/// памяти — тогда условие «был 0, стал 1, снова 0» не сойдётся ни на одном
/// байте либо сойдётся на многих, и находку мы отвергнем.
fn probe_use_item(handles: &Handles, this: *mut c_void) {
    if USE_ITEM_PROBED.swap(true, Ordering::Relaxed) {
        return;
    }
    if this.is_null() || !SINGLE_PLAYER.load(Ordering::Relaxed) {
        USE_ITEM_PROBED.store(false, Ordering::Relaxed);
        return;
    }
    let span = readable_span(this);
    // Первые четыре байта — указатель на таблицу методов, полей там нет.
    if span <= 8 {
        crate::log!("нажатие: объект игрока прочитать не вышло, остаюсь на рефлексии");
        return;
    }

    let bytes = |from: usize| -> Vec<u8> {
        unsafe { std::slice::from_raw_parts(this.cast::<u8>().add(from), span - from).to_vec() }
    };

    let before = bytes(4);
    if !set_use_item(handles, true) {
        return;
    }
    let pressed = bytes(4);
    if !set_use_item(handles, false) {
        return;
    }
    let released = bytes(4);

    let mut found: Option<usize> = None;
    let mut count = 0usize;
    for i in 0..before.len() {
        if before[i] == 0 && pressed[i] == 1 && released[i] == 0 {
            count += 1;
            found = Some(i + 4);
        }
    }
    let Some(offset) = found.filter(|_| count == 1) else {
        crate::log!(
            "нажатие: смещение controlUseItem не опознано (подошло байт: {count}), \
             остаюсь на рефлексии"
        );
        return;
    };

    // Проверка end-to-end: пишем байтом, читаем рефлексией. Совпало в обе
    // стороны — значит нашли именно то поле, а не соседа.
    let check = |want: bool| -> bool {
        unsafe { this.cast::<u8>().add(offset).write(u8::from(want)) };
        handles
            .local_player()
            .and_then(|p| handles.control_use_item.get(&p).ok())
            .and_then(|v| v.as_bool())
            == Some(want)
    };
    if !check(true) || !check(false) {
        crate::log!("нажатие: смещение 0x{offset:X} проверку не прошло, остаюсь на рефлексии");
        return;
    }

    USE_ITEM_OFFSET.store(offset, Ordering::Relaxed);
    crate::log!(
        "нажатие: controlUseItem найден по смещению 0x{offset:X} — \
         дальше ставим байтом, без обращений к CLR"
    );
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
    let mut queue = chat_queue();
    // Если разбирать очередь некому, старые строки уже неинтересны.
    if queue.len() >= CHAT_LIMIT {
        queue.remove(0);
    }
    queue.push(line);
    CHAT_PENDING.store(true, Ordering::Release);
}

/// Очередь чата под замком. Отравленный мьютекс разотравляем: внутри —
/// список строк, после паники он лишь неполон, а не опасен, и молчащий
/// навсегда чат хуже потерянной строки.
fn chat_queue() -> std::sync::MutexGuard<'static, Vec<String>> {
    CHAT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Поставить в очередь звук, см. константы `SOUND_*`. Сыграет его игровой
/// поток в ближайшем кадре, см. `play_sound`.
pub fn request_sound(kind: u8) {
    SOUND.fetch_or(kind, Ordering::Release);
}

/// Играет короткий звук руками игры — чтобы он слушался её громкости
/// и глушился вместе с остальным звуком. Только с игрового потока.
fn play_sound(handles: &Handles, wanted: u8) {
    let Some(api) = handles.sound.as_ref() else {
        return;
    };
    let Ok(player) = api.player.get_static() else {
        return;
    };
    if player.is_null() {
        return;
    }

    // Координаты -1 означают «без привязки к месту»: звук звучит ровно так,
    // как интерфейсный, а не из точки в мире.
    let play = |id: i32, variant: i32| {
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
    };

    if wanted & SOUND_QUEST != 0
        && let Ok(style) = api.style.get_static()
        && !style.is_null()
        && let Some(id) = api.style_id.get(&style).ok().and_then(|v| v.as_int())
    {
        let variant = api
            .style_variant
            .invoke(&style, &[])
            .ok()
            .and_then(|v| v.as_int())
            .unwrap_or(1);
        play(id, variant);
    }
    // У этих двух звук одиночный: игра держит его отдельным полем и вариант
    // при воспроизведении не смотрит вовсе, так что передаём единицу.
    if wanted & SOUND_TICK != 0 {
        play(SOUND_ID_MENU_TICK, 1);
    }
    if wanted & SOUND_CHAT != 0 {
        play(SOUND_ID_CHAT, 1);
    }
    // А тут вариант — настоящий номер: у звуков предметов один тип (2)
    // и много вариантов, и `SoundID.Item3` это `LegacySoundStyle(2, 3)`.
    if wanted & SOUND_DRINK != 0 {
        play(SOUND_ID_ITEM, SOUND_ITEM_DRINK);
    }
}

/// `SoundID.MenuTick` — щелчок при наведении и нажатии в меню игры.
const SOUND_ID_MENU_TICK: i32 = 12;
/// `SoundID.Chat` — звук появления строки в чате.
const SOUND_ID_CHAT: i32 = 24;
/// Тип «звук предмета» и вариант глотка в нём: `SoundID.Item3`.
const SOUND_ID_ITEM: i32 = 2;
const SOUND_ITEM_DRINK: i32 = 3;

/// Отдаёт накопленные строки чату игры. Только с игрового потока: чат —
/// её собственный список, и правит его она сама в своём кадре.
fn flush_chat(handles: &Handles) {
    let lines: Vec<String> = std::mem::take(&mut *chat_queue());
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
///
/// Если нажатие уже было выставлено, просим игровой поток его отпустить:
/// бросать поле нажатым — значит оставлять за собой состояние, которого
/// мы не заводили.
pub fn cancel() {
    if COMMAND.swap(CMD_NONE, Ordering::AcqRel) == CMD_RELEASE {
        RELEASE.store(true, Ordering::Release);
    }
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
    //
    // Параметр у `DisplayAndGetFakeItem` объявлен как `ItemRarityColor`, то
    // есть enum, а мы шлём `Int32` и полагаемся на то, что биндер рефлексии
    // его свернёт. Если не свернёт, подсказок предметов не будет вовсе —
    // и молча, потому что дальше мы просто выходим. Поэтому первый отказ
    // пишем в лог: иначе такую пропажу можно не заметить годами.
    if let Err(e) = api.display.invoke(&Var::null(), &[Var::int(hovered.rare)]) {
        if !TOOLTIP_FAILED.swap(true, Ordering::Relaxed) {
            crate::log!(
                "подсказки предметов недоступны: DisplayAndGetFakeItem не приняла вызов ({e})"
            );
        }
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
