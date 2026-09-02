//! Конфиг в TOML рядом с DLL. Всё, что переключается в UI, живёт здесь.

use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};

use crate::game::POTION_SLOTS;

/// Режим фильтра улова. По умолчанию чёрный список: тянем всё, кроме мусора.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    /// Тянем всё, кроме перечисленного.
    Blacklist,
    /// Тянем только перечисленное.
    Whitelist,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

    /// Разброс задержек перед забросом и подсечкой, мс.
    pub jitter_min_ms: u64,
    pub jitter_max_ms: u64,

    /// Автопитьё: пить самому во время рыбалки.
    pub auto_potions: bool,
    /// Что именно пить, по порядку `game::POTIONS`:
    /// Fishing / Sonar / Crate / Ale / Sake.
    #[serde(deserialize_with = "potion_flags")]
    pub potions: [bool; POTION_SLOTS],

    /// Виртуальные коды клавиш. По умолчанию стрелка вверх, стрелка вниз
    /// и Delete.
    pub hotkey_ui: u16,
    pub hotkey_toggle: u16,
    pub hotkey_unload: u16,

    /// Писать ли в чат о том, что автомат делает. Сообщения видит только
    /// сам игрок: они не уходят на сервер.
    pub chat_messages: bool,
    /// Цвета ярлыков в чате, RGB шестнадцатеричными — как в тегах игры
    /// `[c/RRGGBB:текст]`. Решётка и регистр не важны.
    pub chat_color_blacklist: String,
    pub chat_color_whitelist: String,
    pub chat_color_quest: String,
    pub chat_color_spawn: String,
    pub chat_color_potion: String,
    /// Служебные сообщения автомата: например, что рыбалка остановлена.
    pub chat_color_info: String,
}

/// Читает список ячеек автопитья любой длины.
///
/// В конфигах, написанных до появления эля и сакэ, флагов три. Требовать
/// ровно пять значит объявить весь конфиг битым и молча заменить его
/// значениями по умолчанию — вместе с фильтром улова, который игрок
/// собирал руками. Недостающие ячейки гасим, лишние отбрасываем.
fn potion_flags<'de, D: Deserializer<'de>>(de: D) -> Result<[bool; POTION_SLOTS], D::Error> {
    let list = Vec::<bool>::deserialize(de)?;
    let mut flags = [false; POTION_SLOTS];
    for (flag, value) in flags.iter_mut().zip(list) {
        *flag = value;
    }
    Ok(flags)
}

/// Приводит цвет из конфига к шести шестнадцатеричным цифрам, как ждёт
/// тег игры. Мусор в конфиге не должен ломать сообщение, поэтому на всё
/// непонятное отвечаем белым.
pub fn chat_color(value: &str) -> String {
    let clean: String = value
        .trim()
        .trim_start_matches('#')
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(6)
        .collect();
    if clean.len() == 6 {
        clean.to_ascii_uppercase()
    } else {
        "FFFFFF".to_string()
    }
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
            jitter_min_ms: 120,
            jitter_max_ms: 480,
            auto_potions: false,
            // Эль и сакэ по умолчанию выключены: к рыбалке их бафф идёт
            // в плюс, но за него платят четвёркой защиты, да и в инвентаре
            // они у большинства не лежат. Решает игрок.
            potions: [true, false, true, false, false],
            hotkey_ui: 0x26,     // VK_UP
            hotkey_toggle: 0x28, // VK_DOWN
            hotkey_unload: 0x2E, // VK_DELETE
            chat_messages: true,
            // Чёрный список — почти чёрным: совсем чёрный на тёмном фоне
            // чата не читается.
            chat_color_blacklist: "2A2A2A".to_string(),
            chat_color_whitelist: "FFFFFF".to_string(),
            // Золотистый — тот же, которым игра подписывает квестовую рыбу.
            chat_color_quest: "FFD700".to_string(),
            chat_color_spawn: "FF4040".to_string(),
            chat_color_potion: "D2A0FF".to_string(),
            // Голубой из палитры редкости — им игра красит имена предметов
            // первого уровня, и рядом с остальными ярлыками он читается.
            chat_color_info: "9696FF".to_string(),
        }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Self {
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

    pub fn save(&self, dir: &Path) {
        let path = dir.join("piscatio.toml");
        if let Err(e) = std::fs::write(&path, self.to_commented_toml()) {
            crate::log!("конфиг не сохранён: {e}");
        }
    }

    /// Конфиг с пояснением к каждой строке.
    ///
    /// Собирается вручную, а не `toml::to_string_pretty`: сериализатор
    /// комментариев не пишет, и файл выходил голым списком имён. Панель
    /// пересохраняет конфиг на каждом переключении, поэтому текст должен
    /// восстанавливаться целиком, а не только при создании файла.
    fn to_commented_toml(&self) -> String {
        let mode = match self.filter_mode {
            FilterMode::Blacklist => "blacklist",
            FilterMode::Whitelist => "whitelist",
        };
        let potions = format!(
            "[{}]",
            self.potions
                .iter()
                .map(|on| on.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        format!(
            "# Настройки terraria piscatio automatica.\n\
             # Файл перезаписывается панелью при каждом переключении,\n\
             # так что правки руками делайте при закрытой игре.\n\
             \n\
             # Режим фильтра улова:\n\
             #   blacklist — тяну всё, кроме перечисленного в blacklist;\n\
             #   whitelist — тяну только перечисленное в whitelist.\n\
             filter_mode = \"{mode}\"\n\
             \n\
             # Id предметов, которые НЕ подсекаю в режиме blacklist.\n\
             blacklist = {blacklist}\n\
             \n\
             # Id предметов, которые подсекаю в режиме whitelist.\n\
             whitelist = {whitelist}\n\
             \n\
             # Подсекать ли вражеские спавны — Герцога Рыброна и прочее.\n\
             # На крючке они приходят отрицательным id, фильтр к ним неприменим.\n\
             pull_enemy_spawns = {pull_enemy_spawns}\n\
             \n\
             # Инвентарь заполнился — разложить по ближайшим сундукам.\n\
             # Выключено: при полном инвентаре рыбалка просто останавливается,\n\
             # иначе улов уходил бы в никуда.\n\
             quick_stack_when_full = {quick_stack_when_full}\n\
             \n\
             # Рисовать панель из детура Main.DrawCursor, чтобы игровой курсор\n\
             # ложился поверх неё. Выключить, если панель подозревают в падении:\n\
             # тогда она уйдёт в Present и окажется поверх курсора.\n\
             cursor_detour = {cursor_detour}\n\
             \n\
             # Разброс задержек перед забросом и подсечкой, миллисекунды.\n\
             # Нужен, чтобы действия не шли метрономом.\n\
             jitter_min_ms = {jitter_min_ms}\n\
             jitter_max_ms = {jitter_max_ms}\n\
             \n\
             # Доливать бафы из инвентаря самому. Работает только пока идёт\n\
             # авторыбалка: включена и точка заброса уже зафиксирована.\n\
             auto_potions = {auto_potions}\n\
             \n\
             # Что пить, по порядку: зелья рыбалки (2354), сонара (2355),\n\
             #   ящиков (2356), затем эль (353) и сакэ (2266).\n\
             # Эль и сакэ дают один и тот же бафф Tipsy, так что при двух\n\
             #   включённых пьётся только эль — сакэ ждёт, пока бафф спадёт.\n\
             potions = {potions}\n\
             \n\
             # Виртуальные коды клавиш (VK). По умолчанию стрелка вверх,\n\
             # стрелка вниз и Delete.\n\
             # hotkey_ui     — свернуть и раскрыть панель;\n\
             # hotkey_toggle — включить и выключить авторыбалку;\n\
             # hotkey_unload — выгрузить DLL из игры.\n\
             hotkey_ui = {hotkey_ui}\n\
             hotkey_toggle = {hotkey_toggle}\n\
             hotkey_unload = {hotkey_unload}\n\
             \n\
             # Писать в чат о том, что автомат делает. Сообщения местные:\n\
             # их видит только сам игрок, на сервер они не уходят.\n\
             chat_messages = {chat_messages}\n\
             \n\
             # Цвета ярлыков в чате, RGB шестнадцатеричными — как в тегах игры\n\
             # [c/RRGGBB:текст]. Решётка и регистр не важны, непонятное даёт белый.\n\
             chat_color_blacklist = \"{chat_color_blacklist}\"  # пропуск по чёрному списку\n\
             chat_color_whitelist = \"{chat_color_whitelist}\"  # пропуск по белому списку\n\
             chat_color_quest = \"{chat_color_quest}\"      # квестовая рыба рыбака\n\
             chat_color_spawn = \"{chat_color_spawn}\"      # вражеский спавн\n\
             chat_color_potion = \"{chat_color_potion}\"     # автопитьё зелья\n\
             chat_color_info = \"{chat_color_info}\"       # служебные сообщения автомата\n",
            mode = mode,
            blacklist = int_list(&self.blacklist),
            whitelist = int_list(&self.whitelist),
            pull_enemy_spawns = self.pull_enemy_spawns,
            quick_stack_when_full = self.quick_stack_when_full,
            cursor_detour = self.cursor_detour,
            jitter_min_ms = self.jitter_min_ms,
            jitter_max_ms = self.jitter_max_ms,
            auto_potions = self.auto_potions,
            potions = potions,
            hotkey_ui = self.hotkey_ui,
            hotkey_toggle = self.hotkey_toggle,
            hotkey_unload = self.hotkey_unload,
            chat_messages = self.chat_messages,
            chat_color_blacklist = self.chat_color_blacklist,
            chat_color_whitelist = self.chat_color_whitelist,
            chat_color_quest = self.chat_color_quest,
            chat_color_spawn = self.chat_color_spawn,
            chat_color_potion = self.chat_color_potion,
            chat_color_info = self.chat_color_info,
        )
    }
}

/// Список id в строку TOML.
fn int_list(values: &[i32]) -> String {
    let items: Vec<String> = values.iter().map(|v| v.to_string()).collect();
    format!("[{}]", items.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Конфиг пишется руками, а читается `serde`. Значит, написанное должно
    /// читаться обратно — иначе первое же сохранение сделает файл битым,
    /// и заметит это только пользователь.
    ///
    /// Сравниваем структуры целиком, а не выборочные поля: смысл проверки
    /// в том, чтобы новое поле, забытое в `to_commented_toml`, роняло тест,
    /// а не тихо терялось при каждом сохранении.
    #[test]
    fn commented_toml_reads_back() {
        let written = Config {
            filter_mode: FilterMode::Whitelist,
            whitelist: vec![2290, 2297],
            blacklist: vec![],
            potions: [false, true, false, true, false],
            jitter_min_ms: 7,
            chat_messages: false,
            chat_color_quest: "ABCDEF".to_string(),
            ..Config::default()
        };

        let text = written.to_commented_toml();
        let read: Config = toml::from_str(&text).expect("конфиг не читается обратно");

        assert_eq!(read, written, "поле потерялось при сохранении");
    }

    /// Конфиг с тремя ячейками автопитья писали версии до эля и сакэ.
    /// Строгий массив на пять сделал бы такой файл битым целиком, и игрок
    /// молча потерял бы вместе с ним свой фильтр улова.
    #[test]
    fn old_three_slot_potions_still_load() {
        let text = "potions = [true, false, true]\nblacklist = [2337]\n";
        let read: Config = toml::from_str(text).expect("старый конфиг не читается");

        assert_eq!(read.potions, [true, false, true, false, false]);
        assert_eq!(read.blacklist, vec![2337], "остальное должно уцелеть");
    }

    /// Цвет из конфига должен доезжать до тега игры в шести цифрах,
    /// а мусор — не ломать сообщение.
    #[test]
    fn chat_color_is_forgiving() {
        assert_eq!(chat_color("#ff0000"), "FF0000");
        assert_eq!(chat_color(" 2a2A2a "), "2A2A2A");
        assert_eq!(chat_color("нет"), "FFFFFF");
        assert_eq!(chat_color(""), "FFFFFF");
    }
}
