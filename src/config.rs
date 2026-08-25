//! Конфиг в TOML рядом с DLL. Всё, что переключается в UI, живёт здесь.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Режим фильтра улова. По умолчанию чёрный список: тянем всё, кроме мусора.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    /// Тянем всё, кроме перечисленного.
    Blacklist,
    /// Тянем только перечисленное.
    Whitelist,
}

impl FilterMode {
    #[allow(dead_code)]
    pub fn toggled(self) -> Self {
        match self {
            FilterMode::Blacklist => FilterMode::Whitelist,
            FilterMode::Whitelist => FilterMode::Blacklist,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub filter_mode: FilterMode,
    /// Item id, которые НЕ подсекаем в режиме blacklist.
    /// По умолчанию — мусор: Old Shoe, Seaweed, Tin Can (id взяты из кода игры).
    pub blacklist: Vec<i32>,
    /// Item id, которые подсекаем в режиме whitelist.
    pub whitelist: Vec<i32>,
    /// Подсекать ли вражеские спавны (Дюк Фишрон и прочее). localAI[1] < 0.
    pub pull_enemy_spawns: bool,

    /// Инвентарь заполнен -> эмулируем "разложить по ближайшим сундукам".
    /// Если выключено — просто продолжаем ловить.
    pub quick_stack_when_full: bool,

    /// Рисовать панель из детура `Main.DrawCursor`, чтобы игровой курсор
    /// ложился поверх неё. Выключить — панель уйдёт в `Present`, и курсор
    /// придётся рисовать самим. Оставлено на случай, если детур не поладит
    /// с конкретной сборкой игры.
    pub cursor_detour: bool,

    /// Снять троттлинг игры при потере фокуса (Main.ThrottleWhenInactive = false).
    /// Без этого свёрнутая игра спит по 20 мс на кадр.
    pub disable_inactive_throttle: bool,

    /// Разброс задержек перед забросом и подсечкой, мс.
    pub jitter_min_ms: u64,
    pub jitter_max_ms: u64,

    /// Автопитьё зелий.
    pub auto_potions: bool,
    /// Какие из трёх зелий пить: Fishing / Sonar / Crate.
    pub potions: [bool; 3],

    /// Виртуальные коды клавиш. По умолчанию Insert / End / Delete.
    pub hotkey_ui: u16,
    pub hotkey_toggle: u16,
    pub hotkey_unload: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            filter_mode: FilterMode::Blacklist,
            blacklist: vec![2337, 2338, 2339],
            whitelist: Vec::new(),
            pull_enemy_spawns: false,
            quick_stack_when_full: true,
            cursor_detour: true,
            disable_inactive_throttle: true,
            jitter_min_ms: 120,
            jitter_max_ms: 480,
            auto_potions: false,
            potions: [true, false, true],
            hotkey_ui: 0x26,     // VK_UP
            hotkey_toggle: 0x28, // VK_DOWN
            hotkey_unload: 0x2E, // VK_DELETE
        }
    }
}

impl Config {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("piscatio.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => {
                    crate::log!("конфиг загружен: {}", path.display());
                    cfg
                }
                Err(e) => {
                    crate::log!("конфиг битый ({e}), беру значения по умолчанию");
                    Config::default()
                }
            },
            Err(_) => {
                let cfg = Config::default();
                cfg.save(dir);
                crate::log!("конфиг создан: {}", path.display());
                cfg
            }
        }
    }

    pub fn save(&self, dir: &PathBuf) {
        let path = dir.join("piscatio.toml");
        match toml::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    crate::log!("конфиг не сохранён: {e}");
                }
            }
            Err(e) => crate::log!("конфиг не сериализуется: {e}"),
        }
    }
}
