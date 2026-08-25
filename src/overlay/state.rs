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

#[derive(Default)]
pub struct Status {
    pub connected: String,
    pub fishing: String,
    pub bobber_cast: bool,
    /// Куда забрасываем. `None` — ждём, пока игрок бросит удочку сам.
    pub aim: Option<(i32, i32)>,
    pub free_slots: i32,
    pub bait: i32,
    pub detour_ready: bool,
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
    /// Имена этих предметов в нижнем регистре — под поиск в фильтре.
    /// Спрашиваются у игры один раз, на рабочем потоке.
    pub names: Vec<(i32, String)>,
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
            names: Vec::new(),
            status: Status::default(),
            stats: Stats::default(),
            dirty: false,
        }
    }
}

impl Shared {
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
pub fn with<R>(f: impl FnOnce(&mut Shared) -> R) -> Option<R> {
    let mut guard = SHARED.lock().ok()?;
    Some(f(guard.get_or_insert_with(Shared::default)))
}
