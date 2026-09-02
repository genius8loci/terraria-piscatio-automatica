//! Язык интерфейса: русский, если игра идёт по-русски, иначе английский.
//!
//! Выбор делается один раз при подключении и дальше живёт в атомике: панель
//! рисуется в чужом кадре, и лазить туда за строками через мьютекс незачем.
//! Спрашиваем у самой игры — `Terraria.Localization.Language.ActiveCulture`,
//! у культуры есть `LegacyId`, и русский там шестой (`GameCulture.CultureName`).
//!
//! Переводим только то, что видит игрок: подписи панели, подсказки и строки
//! в чат. Лог остаётся русским — он для нас, а не для игрока.

use std::sync::atomic::{AtomicBool, Ordering};

/// `GameCulture.CultureName.Russian`.
pub const RUSSIAN_ID: i32 = 6;

/// По умолчанию русский: язык узнаём при подключении, а до него панель уже
/// может успеть нарисоваться.
static RUSSIAN: AtomicBool = AtomicBool::new(true);

pub fn set_russian(yes: bool) {
    RUSSIAN.store(yes, Ordering::Relaxed);
}

pub fn is_russian() -> bool {
    RUSSIAN.load(Ordering::Relaxed)
}

/// Строки текущего языка.
pub fn t() -> &'static Strings {
    if is_russian() { &RU } else { &EN }
}

/// Подставляет значения вместо `{}` по порядку. `format!` тут не годится:
/// шаблон приходит из таблицы, а не из исходника.
pub fn fill(template: &str, values: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    for value in values {
        match rest.split_once("{}") {
            Some((before, after)) => {
                out.push_str(before);
                out.push_str(value);
                rest = after;
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// Всё, что видит игрок. Порядок полей — порядок появления в панели.
pub struct Strings {
    // --- основное окно ---
    pub auto_fish: &'static str,
    pub note_wait_cast: &'static str,
    pub note_recast: &'static str,
    /// Шаблон с двумя `{}`: координаты заброса.
    pub note_aim: &'static str,
    /// Приписки к строке авторыбалки, когда автомат остановился сам.
    pub note_stop_no_bait: &'static str,
    pub note_stop_inventory: &'static str,
    pub note_stop_no_chests: &'static str,
    pub note_stop_bobber: &'static str,
    pub quick_stack: &'static str,
    pub pull_enemies: &'static str,
    pub bobber: &'static str,
    pub bobber_cast: &'static str,
    pub bobber_flying: &'static str,
    pub bobber_none: &'static str,
    pub free_slots: &'static str,
    pub auto_potions: &'static str,
    /// Приписка к строке автопитья: включено, но рыбалка ещё не идёт.
    pub note_potions_idle: &'static str,
    pub potions_shelf: &'static str,
    pub tab_filter: &'static str,
    pub tab_stats: &'static str,

    // --- окно фильтра ---
    pub search_hint: &'static str,
    pub hint_no_potion: &'static str,
    pub hint_list_black: &'static str,
    pub hint_list_white: &'static str,

    // --- статистика ---
    pub stat_time: &'static str,
    pub stat_caught: &'static str,
    pub stat_crates: &'static str,
    pub stat_skipped: &'static str,
    pub stat_bite: &'static str,
    pub stat_potions: &'static str,
    /// Шаблон с одним `{}`: секунды с десятыми.
    pub stat_seconds: &'static str,

    // --- сообщения в чат ---
    pub chat_blacklist: &'static str,
    pub chat_whitelist: &'static str,
    pub chat_quest: &'static str,
    pub chat_spawn: &'static str,
    pub chat_potion: &'static str,
    pub chat_info: &'static str,
    /// С одним `{}` — сколько попыток перезаброса сделали.
    pub chat_flight_lost: &'static str,
    /// Все четыре — с одним `{}` под имя и иконку предмета.
    pub chat_item_skipped: &'static str,
    pub chat_quest_caught: &'static str,
    pub chat_spawn_skipped: &'static str,
    pub chat_spawn_hooked: &'static str,
    pub chat_potion_used: &'static str,
}

impl Strings {
    /// Подписи строк основного окна — по самой длинной из них считается
    /// ширина панели. Строк с припиской (авторыбалка, автопитьё) и полки
    /// с ячейками здесь нет: они меряются отдельно, вместе со своей припиской
    /// и рядом ячеек.
    pub fn row_labels(&self) -> [&'static str; 4] {
        [
            self.quick_stack,
            self.pull_enemies,
            self.bobber,
            self.free_slots,
        ]
    }
}

pub static RU: Strings = Strings {
    auto_fish: "Авторыбалка",
    note_wait_cast: " (жду первого броска удочки)",
    note_recast: " (забросьте удочку заново)",
    note_aim: " (зафиксировано {}:{})",
    note_stop_no_bait: " (остановлена: кончилась наживка)",
    note_stop_inventory: " (остановлена: инвентарь полон)",
    note_stop_no_chests: " (остановлена: некуда разложить)",
    note_stop_bobber: " (остановлена: поплавок мимо воды)",
    quick_stack: "Сундуки разложить при заполнении",
    pull_enemies: "Спавнить мобов во время рыбалки",
    bobber: "Поплавок",
    bobber_cast: "Заброшен",
    bobber_flying: "Летит",
    bobber_none: "Нет",
    free_slots: "Свободные ячейки",
    auto_potions: "Автопитьё",
    note_potions_idle: " (ждёт заброса)",
    potions_shelf: "Предметы для автоиспользования:",
    tab_filter: "Фильтр",
    tab_stats: "Статистика",

    search_hint: "Имя:",
    hint_no_potion: "Этого предмета нет в инвентаре.\nПоложите его в инвентарь, чтобы включить автопитьё.",
    hint_list_black: "Чёрный список: беру всё,\nкроме отмеченного красным.\nЩелчок — сменить на белый.",
    hint_list_white: "Белый список: беру только\nотмеченное зелёным.\nЩелчок — сменить на чёрный.",

    stat_time: "Время рыбалки",
    stat_caught: "Поймано предметов",
    stat_crates: "Поймано ящиков",
    stat_skipped: "Пропущено по фильтру",
    stat_bite: "Среднее время поклёвки",
    stat_potions: "Выпито зелий и еды",
    stat_seconds: "{} сек.",

    chat_blacklist: "Чёрный список",
    chat_whitelist: "Белый список",
    chat_quest: "Квест рыбака",
    chat_spawn: "Спавн",
    chat_potion: "Автопитьё",
    chat_info: "Инфо",
    chat_flight_lost: "поплавок не достиг воды после {} попыток, авторыбалка остановлена",
    chat_item_skipped: "пропущен предмет {}",
    chat_quest_caught: "поймана квестовая рыба {}",
    chat_spawn_skipped: "пропущен спавн {}",
    chat_spawn_hooked: "заспавнен {}",
    chat_potion_used: "авто-использовано {}",
};

pub static EN: Strings = Strings {
    auto_fish: "Auto-fishing",
    note_wait_cast: " (waiting for your first cast)",
    note_recast: " (cast the rod again)",
    note_aim: " (locked at {}:{})",
    note_stop_no_bait: " (stopped: out of bait)",
    note_stop_inventory: " (stopped: inventory full)",
    note_stop_no_chests: " (stopped: nowhere to stack)",
    note_stop_bobber: " (stopped: bobber misses water)",
    quick_stack: "Stack into nearby chests when full",
    pull_enemies: "Spawn mobs while fishing",
    bobber: "Bobber",
    bobber_cast: "In the water",
    bobber_flying: "In flight",
    bobber_none: "None",
    free_slots: "Free slots",
    auto_potions: "Auto-drink",
    note_potions_idle: " (waiting for the cast)",
    potions_shelf: "Items to use automatically:",
    tab_filter: "Filter",
    tab_stats: "Statistics",

    search_hint: "Name:",
    hint_no_potion: "You have none of this item.\nPut one in your inventory to enable auto-drinking.",
    hint_list_black: "Blacklist: keeping everything\nexcept what is marked red.\nClick to switch to a whitelist.",
    hint_list_white: "Whitelist: keeping only\nwhat is marked green.\nClick to switch to a blacklist.",

    stat_time: "Time fishing",
    stat_caught: "Items caught",
    stat_crates: "Crates caught",
    stat_skipped: "Skipped by filter",
    stat_bite: "Average time to bite",
    stat_potions: "Potions and food drunk",
    stat_seconds: "{} sec.",

    chat_blacklist: "Blacklist",
    chat_whitelist: "Whitelist",
    chat_quest: "Angler quest",
    chat_spawn: "Spawn",
    chat_potion: "Auto-drink",
    chat_info: "Info",
    chat_flight_lost: "the bobber never reached water after {} tries, auto-fishing stopped",
    chat_item_skipped: "skipped {}",
    chat_quest_caught: "caught the quest fish {}",
    chat_spawn_skipped: "skipped the spawn {}",
    chat_spawn_hooked: "hooked {}",
    chat_potion_used: "auto-used {}",
};
