//! Состояние, общее для рабочего потока и потока рендера.
//!
//! Рабочий поток пишет сюда показания игры, UI — переключатели.
//! Мьютекс берётся короткими кусками, поэтому кадру он не мешает.

use std::collections::HashMap;
use std::sync::Mutex;

/// Отметка предмета в фильтре.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// Нейтрально — решает режим списка.
    Neutral,
    /// Зелёный: берём.
    Allow,
    /// Красный: пропускаем.
    Deny,
}

impl Mark {
    /// Клик по ячейке: нейтрально -> берём -> пропускаем -> нейтрально.
    pub fn next(self) -> Self {
        match self {
            Mark::Neutral => Mark::Allow,
            Mark::Allow => Mark::Deny,
            Mark::Deny => Mark::Neutral,
        }
    }
}

/// Что игра рассказала про предмет: имя в двух видах и признак квестовой
/// рыбы. Поиску нужен нижний регистр, чату — как показывает игра.
pub struct ItemFacts {
    pub search: String,
    pub display: String,
    pub quest: bool,
}

/// Где сейчас поплавок. «Летит» — снаряд уже есть, но воды ещё не коснулся:
/// это `Projectile.wet` у самой игры.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bobber {
    #[default]
    None,
    Flying,
    InWater,
}

/// Почему автомат остановился сам.
///
/// Код, а не строка: причину показывает панель, а она рисуется на языке
/// игры. Текст для лога остаётся русским и живёт отдельно, см.
/// `Fishing::status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stop {
    #[default]
    None,
    /// Наживка кончилась.
    NoBait,
    /// Инвентарь полон, раскладка по сундукам выключена.
    InventoryFull,
    /// Инвентарь полон, а складывать некуда.
    NoChests,
    /// Поплавок так и не долетел до воды.
    BobberStuck,
}

#[derive(Default)]
pub struct Status {
    pub connected: String,
    pub fishing: String,
    pub bobber: Bobber,
    /// Куда забрасываем. `None` — ждём, пока игрок бросит удочку сам.
    pub aim: Option<(i32, i32)>,
    /// Включились, когда поплавок уже лежал в воде: точку заброса по нему
    /// не взять, курсор к тому времени где угодно. Ждём нового заброса.
    pub recast: bool,
    /// Автомат остановился сам — панель говорит, почему.
    pub stop: Stop,
    pub free_slots: i32,
    pub detour_ready: bool,
    /// Каких из трёх зелий сейчас нет в инвентаре. Именно «нет», а не «есть»:
    /// до первого опроса игры здесь нули, и панель молчит вместо того, чтобы
    /// перечеркнуть все три ячейки на пустом месте.
    pub potions_missing: [bool; 3],
}

#[derive(Default)]
pub struct Stats {
    pub seconds: u64,
    pub caught: u32,
    pub crates: u32,
    pub potions: u32,
    pub skipped: u32,
    pub average_bite: f32,
}

pub struct Shared {
    pub auto_fish: bool,
    pub quick_stack: bool,
    pub auto_potions: bool,
    /// Подсекать ли вражеские спавны: Герцог Рыброн и прочая нежить.
    pub pull_enemy_spawns: bool,
    pub whitelist_mode: bool,
    /// Какие из трёх зелий пить.
    pub potions: [bool; 3],
    pub filter: HashMap<i32, Mark>,
    /// Что вообще ловится — берётся из `Main.FishDropsDB`.
    pub fishable: Vec<i32>,
    /// Что игра знает про эти предметы. Спрашивается один раз при
    /// подключении, на рабочем потоке.
    ///
    /// Таблица, а не список: поиск в фильтре спрашивает про каждый из ста
    /// с лишним предметов на каждом кадре, и перебором это выходило
    /// квадратично прямо в отрисовке.
    pub names: HashMap<i32, ItemFacts>,
    pub status: Status,
    pub stats: Stats,
    /// UI что-то переключил — рабочему потоку надо сохранить конфиг.
    pub dirty: bool,
}

impl Default for Shared {
    fn default() -> Self {
        Shared {
            auto_fish: false,
            quick_stack: true,
            auto_potions: false,
            pull_enemy_spawns: false,
            whitelist_mode: false,
            potions: [true, false, true],
            filter: HashMap::new(),
            fishable: Vec::new(),
            names: HashMap::new(),
            status: Status::default(),
            stats: Stats::default(),
            dirty: false,
        }
    }
}

impl Shared {
    /// Что известно про предмет. `None` — игра о нём не рассказывала.
    pub fn facts(&self, item: i32) -> Option<&ItemFacts> {
        self.names.get(&item)
    }

    /// Решение по улову с учётом режима списка и отметки предмета.
    pub fn should_pull(&self, item: i32) -> bool {
        // Отрицательное значение — не предмет, а вражеский спавн: игра кладёт
        // в `localAI[1]` минус id NPC. Фильтр по иконкам к ним неприменим.
        if item < 0 {
            return self.pull_enemy_spawns;
        }
        match self.filter.get(&item).copied().unwrap_or(Mark::Neutral) {
            Mark::Allow => true,
            Mark::Deny => false,
            // Нейтральные предметы: в белом списке пропускаем, в чёрном берём.
            Mark::Neutral => !self.whitelist_mode,
        }
    }
}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

/// Короткий доступ под мьютексом.
///
/// Отравленный мьютекс разотравляем: внутри — переключатели и показания
/// игры, после паники они лишь неполны. Отказ же брать замок означал бы,
/// что панель навсегда уходит в значения по умолчанию — с выключенной
/// рыбалкой и пустым фильтром, — и понять почему было бы нечем.
///
/// Возвращаемый `Option` оставлен ради вызывающих: он больше не может быть
/// `None`, но и переписывать полсотни мест ради этого незачем.
pub fn with<R>(f: impl FnOnce(&mut Shared) -> R) -> Option<R> {
    let mut guard = SHARED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Some(f(guard.get_or_insert_with(Shared::default)))
}
