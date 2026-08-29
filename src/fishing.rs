//! Автомат рыбалки.
//!
//! Решения принимаются здесь, на рабочем потоке, а нажатие применяет детур
//! `Player.ItemCheck` (см. `input`). Точка заброса запоминается по первому
//! ручному забросу игрока и дальше переиспользуется.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::chat;
use crate::config::Config;
use crate::crash;
use crate::detour;
use crate::game::{Game, POTIONS};
use crate::input;
use crate::log;
use crate::overlay::state;

/// Пауза после подсечки перед повторным забросом.
const AFTER_PULL: u64 = 350;
/// Пауза после заброса, пока поплавок летит.
const AFTER_CAST: u64 = 700;
/// Пауза после раскладки по сундукам: игре нужен кадр на саму раскладку
/// и ещё один опрос, чтобы стало видно освободившиеся ячейки.
const AFTER_STACK: u64 = 700;
/// Сколько раскладок подряд терпим, прежде чем признать, что складывать
/// некуда: сундуков рядом нет либо они полны.
const STACK_TRIES: u32 = 3;
/// Сколько ждать применения нажатия, прежде чем считать его потерянным.
const CLICK_TIMEOUT: Duration = Duration::from_millis(1200);
/// Как часто перебирать снаряды, когда поплавка нет.
///
/// Полный перебор — пара тысяч обращений к CLR, и при пустой воде он шёл
/// восемь раз в секунду впустую. Торопиться там некуда: после заброса
/// автомат всё равно выжидает `AFTER_CAST`, а ручной заброс игрока
/// подождёт треть секунды.
const IDLE_SCAN: Duration = Duration::from_millis(360);
/// Как часто перечитывать наживку и слоты. Число свободных ячеек видно
/// в панели, и раз в пару секунд оно заметно отставало от инвентаря.
const STOCK_INTERVAL: Duration = Duration::from_millis(300);
/// Как часто проверять бафы зелий.
const POTION_INTERVAL: Duration = Duration::from_secs(3);

/// Зачем поставлено нажатие.
///
/// Счётчики и сообщения в чат ждут, пока игра его действительно применит.
/// Раньше и заброс, и подсечка попадали в статистику сразу при постановке
/// команды: при свёрнутом окне, где не проходило ни одно нажатие, панель
/// бодро показывала полторы сотни забросов и ни одного улова.
#[derive(Clone, Copy)]
enum Pending {
    Cast { x: i32, y: i32 },
    Pull { rolled: i32 },
}

/// Поставленное и ещё не разрешившееся нажатие.
struct Sent {
    at: Instant,
    /// Сколько нажатий игра применила к мгновению постановки. Команда могла
    /// сняться и сама (детур не поднял хэндлы), поэтому «применилось» —
    /// это именно рост счётчика, а не просто «перестало быть занято».
    clicks: u32,
    what: Pending,
}

pub struct Fishing {
    /// Экранные координаты первого заброса.
    aim: Option<(i32, i32)>,
    /// Ждём именно нового заброса: тот поплавок, что сейчас в воде, был
    /// заброшен до включения, и курсор с тех пор уехал.
    wait_recast: bool,
    next_action: Instant,
    hint: Option<i32>,
    last_bite: i32,
    stopped: Option<String>,
    rng: u32,
    click_sent: Option<Sent>,
    /// Раньше какого мгновения не перебирать снаряды заново, см. `IDLE_SCAN`.
    next_scan: Instant,
    last_stock: Instant,
    last_potion: Instant,
    warned_no_detour: bool,
    /// Сколько раскладок по сундукам подряд не освободили ни одной ячейки.
    stack_tries: u32,
    /// Когда отправили заявку на раскладку — по ней снимаем зависшую.
    stack_sent: Option<Instant>,

    /// Когда включили рыбалку — от этого мгновения идёт время в статистике.
    /// Пока выключена, показываем последнее набежавшее, а новый запуск
    /// начинает счёт заново.
    session_start: Option<Instant>,
    session_seconds: u64,
    /// Когда поплавок ушёл в воду — от этого мгновения меряем поклёвку.
    cast_at: Option<Instant>,
    /// Сумма и число замеров: среднее считаем по ним, а не по последнему.
    bite_total: f32,
    bite_count: u32,

    pub casts: u32,
    pub pulls: u32,
    pub skips: u32,
    pub crates: u32,
}

/// Зерно для разброса задержек.
///
/// С зашитым зерном «разброс» повторялся от запуска к запуску одной и той же
/// последовательностью — то есть оставался метрономом, просто длинным.
fn seed() -> u32 {
    let value = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ d.as_secs() as u32)
        .unwrap_or(0);
    // Xorshift не выбирается из нулевого состояния.
    if value == 0 { 0x9E37_79B9 } else { value }
}

impl Fishing {
    pub fn new() -> Self {
        Fishing {
            aim: None,
            wait_recast: false,
            next_action: Instant::now(),
            hint: None,
            last_bite: 0,
            stopped: None,
            rng: seed(),
            click_sent: None,
            next_scan: Instant::now(),
            last_stock: Instant::now() - STOCK_INTERVAL,
            last_potion: Instant::now() - POTION_INTERVAL,
            warned_no_detour: false,
            stack_tries: 0,
            stack_sent: None,
            session_start: None,
            session_seconds: 0,
            cast_at: None,
            bite_total: 0.0,
            bite_count: 0,
            casts: 0,
            pulls: 0,
            skips: 0,
            crates: 0,
        }
    }

    fn enabled(&self) -> bool {
        state::with(|s| s.auto_fish).unwrap_or(false)
    }

    /// Ведёт время рыбалки: счёт идёт от включения, а не от инжекта.
    /// Выключили — время замирает на последнем значении; включили снова —
    /// начинается заново.
    ///
    /// Заодно на каждом запуске забывается точка заброса. Иначе неудачный
    /// первый бросок запоминался навсегда, и автомат долбил в ту же
    /// неудачную точку даже после перезапуска.
    ///
    /// И поднимается `wait_recast`: включить могли при уже заброшенном
    /// поплавке, а курсор к этому мгновению стоит где угодно — обычно
    /// прямо на переключателе в панели. Взять по нему точку заброса значит
    /// взять заведомо неверную. Флаг снимается там же, где выясняется, что
    /// поплавка в воде нет (см. `tick`), так что при пустой воде он живёт
    /// доли тика и ничего не задерживает.
    fn track_session(&mut self) {
        let enabled = self.enabled();
        match (enabled, self.session_start) {
            (true, None) => {
                self.session_start = Some(Instant::now());
                self.session_seconds = 0;
                self.aim = None;
                self.wait_recast = true;
                self.stopped = None;
                self.stack_tries = 0;
                // Счётчики идут с тем же нуля, что и время: иначе панель
                // показывала свежие секунды рядом с уловом прошлой сессии.
                self.casts = 0;
                self.pulls = 0;
                self.skips = 0;
                self.crates = 0;
                self.bite_total = 0.0;
                self.bite_count = 0;
                self.cast_at = None;
                self.last_bite = 0;
                state::with(|s| s.stats.potions = 0);
                log!("рыбалка включена: жду первый заброс вручную");
            }
            (true, Some(start)) => self.session_seconds = start.elapsed().as_secs(),
            (false, Some(_)) => {
                self.session_start = None;
                self.aim = None;
                self.wait_recast = false;
            }
            (false, None) => {}
        }
    }

    /// Строка состояния для лога и панели.
    pub fn status(&self) -> String {
        if let Some(reason) = &self.stopped {
            return format!("стоп: {reason}");
        }
        if !self.enabled() {
            return "выключена".to_string();
        }
        if !detour::is_active() {
            return "детур не стоит — нажимать некому".to_string();
        }
        if self.wait_recast {
            return "поплавок уже был в воде — жду нового заброса".to_string();
        }
        if self.aim.is_none() {
            return "жду первый заброс вручную".to_string();
        }
        format!(
            "работает | забросов {} уловов {} пропущено {}",
            self.casts, self.pulls, self.skips
        )
    }

    fn jitter(&mut self, config: &Config, base: u64) -> Duration {
        // Xorshift: своего генератора достаточно, тянуть зависимость незачем.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        // Границы могли приехать из конфига перепутанными: разбираем их
        // по месту, а не выдаём молча минимум.
        let lo = config.jitter_min_ms.min(config.jitter_max_ms);
        let hi = config.jitter_min_ms.max(config.jitter_max_ms);
        let extra = lo + (self.rng as u64) % (hi - lo + 1);
        Duration::from_millis(base + extra)
    }

    /// Ставит нажатие в очередь. Без детура применять его некому.
    fn click(&mut self, aim: Option<(i32, i32)>, what: Pending) -> bool {
        if !detour::is_active() {
            if !self.warned_no_detour {
                self.warned_no_detour = true;
                log!("рыбалка: детур не стоит, нажать некому — автомат простаивает");
            }
            return false;
        }
        // Счётчик снимаем до постановки: игровой поток мог бы успеть
        // применить нажатие между этими строками, и своё же нажатие
        // мы бы потом не узнали.
        let clicks = input::CLICKS.load(Ordering::Relaxed);
        input::request_click(aim);
        self.click_sent = Some(Sent {
            at: Instant::now(),
            clicks,
            what,
        });
        true
    }

    /// Разбирает судьбу поставленного нажатия.
    ///
    /// Применилось — только здесь растут счётчики и уходит строка в чат.
    /// Не применилось за отведённое время — снимаем команду сами, иначе
    /// `busy()` навсегда останется истинным и автомат встанет.
    fn settle_click(&mut self, game: &Game, config: &Config) {
        let Some(sent) = self.click_sent.take() else {
            return;
        };
        if input::busy() {
            if sent.at.elapsed() > CLICK_TIMEOUT {
                input::cancel();
                log!("ввод: нажатие не применилось за {CLICK_TIMEOUT:?}, снял команду");
            } else {
                self.click_sent = Some(sent);
            }
            return;
        }
        if input::CLICKS.load(Ordering::Relaxed) == sent.clicks {
            log!("ввод: команда снялась, не применившись");
            return;
        }
        match sent.what {
            Pending::Cast { x, y } => {
                self.casts += 1;
                log!("заброс #{} в точку {x},{y}", self.casts);
            }
            Pending::Pull { rolled } => {
                self.pulls += 1;
                // Что именно ящик, знает игра: `ItemID.Sets.IsFishingCrate`.
                if game.is_crate(rolled) {
                    self.crates += 1;
                }
                log!("подсечка #{}: улов {rolled}", self.pulls);
                self.announce(game, config, rolled, true);
            }
        }
    }

    pub fn tick(&mut self, game: &Game, config: &Config) {
        self.settle_click(game, config);
        self.track_session();
        self.check_rod(game);

        if self.last_stock.elapsed() >= STOCK_INTERVAL {
            self.last_stock = Instant::now();
            self.refresh_stock(game);
        }
        if self.last_potion.elapsed() >= POTION_INTERVAL {
            self.last_potion = Instant::now();
            self.drink_potions(game, config);
        }

        // Пока поплавка нет, полный перебор снарядов держим на поводке:
        // он стоит тысяч обращений к CLR, а нового поплавка раньше времени
        // всё равно не появится.
        let bobber = if self.hint.is_none() && Instant::now() < self.next_scan {
            Ok(None)
        } else {
            let step = crash::Step::worker(crash::STEP_BOBBER);
            let found = game.find_bobber(self.hint);
            drop(step);
            if matches!(found, Ok(None)) {
                self.next_scan = Instant::now() + IDLE_SCAN;
            }
            found
        };
        match bobber {
            Ok(Some(bobber)) => {
                self.hint = Some(bobber.index);
                let where_is = if bobber.wet {
                    state::Bobber::InWater
                } else {
                    state::Bobber::Flying
                };
                state::with(|s| s.status.bobber = where_is);
                self.remember_aim(game);

                if bobber.has_bite() {
                    self.on_bite(game, config, bobber.rolled());
                } else {
                    self.last_bite = 0;
                    // Поплавок в воде и пока молчит — отсюда и пойдёт отсчёт
                    // до поклёвки, если он ещё не начался.
                    self.cast_at.get_or_insert_with(Instant::now);
                }
            }
            Ok(None) => {
                self.hint = None;
                // Воды без поплавка достаточно: следующий, который в ней
                // появится, заброшен уже при включённой рыбалке, и курсор
                // в это мгновение стоит там, куда игрок целился.
                self.wait_recast = false;
                state::with(|s| s.status.bobber = state::Bobber::None);
                self.on_idle(game, config);
            }
            Err(e) => log!("рыбалка: чтение поплавка не удалось: {e}"),
        }

        let status = self.status();
        let average = if self.bite_count == 0 {
            0.0
        } else {
            self.bite_total / self.bite_count as f32
        };
        state::with(|s| {
            s.status.fishing = status;
            s.status.aim = self.aim;
            s.status.recast = self.wait_recast;
            s.stats.seconds = self.session_seconds;
            s.stats.caught = self.pulls;
            s.stats.skipped = self.skips;
            s.stats.crates = self.crates;
            s.stats.average_bite = average;
        });
    }

    /// Удочку могли убрать из рук колесом или цифрой хотбара. Продолжать
    /// после этого нельзя: автомат будет махать тем, что оказалось в руке,
    /// поэтому просто выключаем режим — так же, как если бы игрок щёлкнул
    /// переключатель сам.
    ///
    /// Смотрим только когда точка заброса уже запомнена: до первого ручного
    /// заброса игрок вправе держать что угодно, рыбалка ещё не началась,
    /// и выключаться на этом было бы вредно.
    fn check_rod(&mut self, game: &Game) {
        if !self.enabled() || self.aim.is_none() {
            return;
        }
        let _step = crash::Step::worker(crash::STEP_ROD);
        let Ok(Some(player)) = game.local_player() else {
            return;
        };
        match game.holding_rod(&player) {
            Ok(true) => {}
            Ok(false) => {
                log!("в руке больше не удочка — авторыбалка выключена");
                state::with(|s| {
                    s.auto_fish = false;
                    s.dirty = true;
                });
            }
            Err(e) => log!("рыбалка: предмет в руке прочитать не удалось: {e}"),
        }
    }

    /// Первый заброс делает игрок — оттуда и берём точку прицела.
    ///
    /// Тот поплавок, что лежал в воде до включения, не в счёт: курсор с его
    /// заброса давно уехал, и точка вышла бы заведомо неверной.
    fn remember_aim(&mut self, game: &Game) {
        // Пока рыбалка выключена, точка не нужна и всё равно забудется при
        // следующем включении: без этой проверки в лог падала строка
        // «точка заброса запомнена» на чужой, ручной заброс игрока.
        if !self.enabled() || self.aim.is_some() || self.wait_recast {
            return;
        }
        let _step = crash::Step::worker(crash::STEP_AIM);
        match game.mouse() {
            Ok((x, y)) if x >= 0 && y >= 0 => {
                self.aim = Some((x, y));
                log!("точка заброса запомнена: {x},{y}");
            }
            Ok(_) => {}
            Err(e) => log!("рыбалка: курсор прочитать не удалось: {e}"),
        }
    }

    fn refresh_stock(&mut self, game: &Game) -> (i32, i32) {
        let _step = crash::Step::worker(crash::STEP_STOCK);
        let Ok(Some(player)) = game.local_player() else {
            state::with(|s| {
                s.status.bait = -1;
                s.status.free_slots = -1;
                s.status.potions_missing = [false; 3];
            });
            return (-1, -1);
        };
        let bait = game.bait_total(&player).unwrap_or(-1);
        let free = game.free_slots(&player).unwrap_or(-1);
        // Зелья пересчитываем здесь же: панель гасит ячейки тех, что кончились,
        // и делать это надо на ходу — запас тает прямо во время рыбалки.
        let mut missing = [false; 3];
        for (index, (item, _, _)) in POTIONS.iter().enumerate() {
            missing[index] = matches!(game.find_item(&player, *item), Ok(None));
        }
        state::with(|s| {
            s.status.bait = bait;
            s.status.free_slots = free;
            s.status.potions_missing = missing;
        });
        (bait, free)
    }

    /// Автопитьё: доливаем только те бафы, что выбраны в панели и погасли.
    fn drink_potions(&mut self, game: &Game, config: &Config) {
        let _step = crash::Step::worker(crash::STEP_POTIONS);
        let (enabled, selected) =
            state::with(|s| (s.auto_potions, s.potions)).unwrap_or((false, [false; 3]));
        if !enabled {
            return;
        }
        let Ok(Some(player)) = game.local_player() else {
            return;
        };
        for (index, (item, buff, name)) in POTIONS.iter().enumerate() {
            if !selected[index] {
                continue;
            }
            if game.has_buff(&player, *buff).unwrap_or(true) {
                continue;
            }
            let Ok(Some(slot)) = game.find_item(&player, *item) else {
                continue;
            };
            match game.drink(&player, slot, *buff) {
                Ok(()) => {
                    log!("выпито зелье {name}");
                    state::with(|s| s.stats.potions += 1);
                    let shown = Self::display_name(*item);
                    chat::potion_used(config, *item, &shown);
                }
                Err(e) => log!("зелье {name} выпить не вышло: {e}"),
            }
        }
    }

    fn on_bite(&mut self, game: &Game, config: &Config, rolled: i32) {
        if rolled == self.last_bite {
            return;
        }
        let _step = crash::Step::worker(crash::STEP_BITE);

        // Замер делаем на первом же обнаружении поклёвки, независимо от того,
        // будем подсекать или нет: ждали-то мы её в любом случае. `take()`
        // сам по себе однократен, так что повтор замера не грозит.
        if let Some(cast) = self.cast_at.take() {
            self.bite_total += cast.elapsed().as_secs_f32();
            self.bite_count += 1;
        }

        let take = state::with(|s| s.should_pull(rolled)).unwrap_or(true);
        if !take {
            self.last_bite = rolled;
            self.skips += 1;
            log!("пропуск: улов {rolled} не проходит фильтр, наживка не тратится");
            self.announce(game, config, rolled, false);
            return;
        }
        if !self.enabled() {
            self.last_bite = rolled;
            log!("поклёвка: улов {rolled} прошёл бы фильтр, но рыбалка выключена");
            return;
        }
        // Заняты или ещё не вышла пауза — поклёвку **не** помечаем разобранной:
        // окно подсечки открыто, попробуем на следующем тике. Раньше отметка
        // ставилась выше, и такая рыба пропадала совсем — до конца окна к ней
        // больше не возвращались.
        if input::busy() || Instant::now() < self.next_action {
            return;
        }
        if !self.click(None, Pending::Pull { rolled }) {
            return;
        }
        self.last_bite = rolled;
        let pause = self.jitter(config, AFTER_PULL);
        self.next_action = Instant::now() + pause;
    }

    /// Рассказывает игроку в чат, что случилось с этой поклёвкой.
    ///
    /// Отрицательный `rolled` — не предмет, а вражеский спавн: игра кладёт
    /// в `localAI[1]` минус id NPC. Про обычный улов пишем только когда он
    /// пропущен или это квестовая рыба: остальное игрок и так видит.
    fn announce(&self, game: &Game, config: &Config, rolled: i32, hooked: bool) {
        if rolled < 0 {
            let name = game
                .npc_name(-rolled)
                .unwrap_or_else(|| format!("#{}", -rolled));
            chat::spawn(config, &name, hooked);
            return;
        }
        let (name, quest) = state::with(|s| {
            s.facts(rolled)
                .map(|f| (f.display.clone(), f.quest))
                .unwrap_or_else(|| (format!("#{rolled}"), false))
        })
        .unwrap_or_else(|| (format!("#{rolled}"), false));
        if !hooked {
            let whitelist = state::with(|s| s.whitelist_mode).unwrap_or(false);
            chat::item_skipped(config, rolled, &name, whitelist);
        } else if quest {
            chat::quest_caught(config, rolled, &name);
            // Квестовая рыба попадается редко, и её легко проглядеть в чате.
            // Звук играет сама игра, чтобы он слушался её громкости.
            input::request_sound();
        }
    }

    /// Имя предмета так, как его показывает игра. Спрошено один раз при
    /// подключении; для чего имени нет — покажем хотя бы id.
    fn display_name(item: i32) -> String {
        state::with(|s| s.facts(item).map(|f| f.display.clone()))
            .flatten()
            .unwrap_or_else(|| format!("#{item}"))
    }

    fn on_idle(&mut self, game: &Game, config: &Config) {
        if !self.enabled() || self.stopped.is_some() {
            return;
        }
        let Some(aim) = self.aim else {
            return;
        };
        if input::busy() || Instant::now() < self.next_action {
            return;
        }

        let (bait, free) = self.refresh_stock(game);
        self.last_stock = Instant::now();
        if bait < 0 {
            return;
        }
        if bait == 0 {
            self.stopped = Some("наживка кончилась".to_string());
            log!("рыбалка остановлена: наживка кончилась");
            return;
        }

        // Забрасывать в полный инвентарь бессмысленно: улов просто пропадёт.
        if free == 0 {
            let quick_stack = state::with(|s| s.quick_stack).unwrap_or(false);
            if !quick_stack {
                self.stopped = Some("инвентарь полон".to_string());
                log!("рыбалка остановлена: инвентарь полон, раскладка по сундукам выключена");
                return;
            }
            // Раскладку делает игровой поток, здесь только заявка. Пока она
            // не исполнена — ждём, ячейки освободятся не сию секунду.
            if input::quick_stack_pending() {
                // Не исполнилась вовсе — значит, детур не сработал. Снимаем,
                // иначе автомат остался бы ждать её навсегда.
                if self
                    .stack_sent
                    .is_some_and(|at| at.elapsed() > CLICK_TIMEOUT)
                {
                    input::cancel_quick_stack();
                    self.stack_sent = None;
                    log!("сундуки: раскладка не исполнилась за {CLICK_TIMEOUT:?}, снял заявку");
                }
                return;
            }
            self.stack_sent = None;
            self.stack_tries += 1;
            if self.stack_tries > STACK_TRIES {
                self.stopped = Some("инвентарь полон, сундуков рядом нет".to_string());
                log!(
                    "рыбалка остановлена: {STACK_TRIES} раскладок подряд не освободили ни одной ячейки"
                );
                return;
            }
            input::request_quick_stack();
            self.stack_sent = Some(Instant::now());
            self.next_action = Instant::now() + Duration::from_millis(AFTER_STACK);
            return;
        }
        self.stack_tries = 0;

        let cast = Pending::Cast { x: aim.0, y: aim.1 };
        if !self.click(Some(aim), cast) {
            return;
        }
        // Поплавок появится в ближайшем тике — перебор снарядов больше
        // не придерживаем.
        self.next_scan = Instant::now();
        let pause = self.jitter(config, AFTER_CAST);
        self.next_action = Instant::now() + pause;
    }
}
