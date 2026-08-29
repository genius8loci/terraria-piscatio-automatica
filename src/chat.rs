//! Сообщения в чат игры о том, что делает автомат.
//!
//! Пишем языком самой игры: `[c/RRGGBB:текст]` красит кусок, `[i:id]`
//! вставляет предмет — с иконкой и той же подсказкой, что в инвентаре.
//! Разбирает эти теги `ChatManager.ParseMessage`, то есть всё оформление
//! делает игра, а не мы.
//!
//! Сообщение никуда не уходит: `chatMonitor` — местный список строк,
//! его видит только сам игрок. Отправка идёт с игрового потока,
//! см. `input::queue_chat`.

use crate::config::{Config, chat_color};
use crate::input;
use crate::lang;

/// Ярлык в квадратных скобках своим цветом, за ним — что случилось.
///
/// Тегов два, а не один, из-за разбора игры: в `ChatManager.Regexes.Format`
/// текст тега — `(?<text>.+?)` до первой же неэкранированной `]`, так что
/// закрывающая скобка ярлыка обрывает тег и остаётся некрашеной. Поэтому
/// она уезжает в начало второго тега, вместе с тире.
fn line(color: &str, label: &str, body: &str) -> String {
    let hex = chat_color(color);
    format!("[c/{hex}:[{label}][c/{hex}:] —] {body}")
}

/// Предмет в строке: имя и следом иконка.
fn item(id: i32, name: &str) -> String {
    format!("{name} [i:{id}]")
}

/// Улов не прошёл фильтр.
pub fn item_skipped(config: &Config, id: i32, name: &str, whitelist: bool) {
    if !config.chat_messages {
        return;
    }
    let t = lang::t();
    let (color, label) = if whitelist {
        (&config.chat_color_whitelist, t.chat_whitelist)
    } else {
        (&config.chat_color_blacklist, t.chat_blacklist)
    };
    let body = lang::fill(t.chat_item_skipped, &[&item(id, name)]);
    input::queue_chat(line(color, label, &body));
}

/// Попалась квестовая рыба — та, что нужна рыбаку.
pub fn quest_caught(config: &Config, id: i32, name: &str) {
    if !config.chat_messages {
        return;
    }
    let t = lang::t();
    let body = lang::fill(t.chat_quest_caught, &[&item(id, name)]);
    input::queue_chat(line(&config.chat_color_quest, t.chat_quest, &body));
}

/// На крючке не предмет, а вражеский спавн. Подсекли или пропустили —
/// зависит от переключателя в панели.
pub fn spawn(config: &Config, name: &str, hooked: bool) {
    if !config.chat_messages {
        return;
    }
    let t = lang::t();
    let template = if hooked {
        t.chat_spawn_hooked
    } else {
        t.chat_spawn_skipped
    };
    let body = lang::fill(template, &[name]);
    input::queue_chat(line(&config.chat_color_spawn, t.chat_spawn, &body));
}

/// Поплавок так и не долетел до воды — автомат сдался.
///
/// Без предмета в строке: сообщать не о чём, кроме самой остановки.
pub fn flight_lost(config: &Config, tries: u32) {
    if !config.chat_messages {
        return;
    }
    let t = lang::t();
    let body = lang::fill(t.chat_flight_lost, &[&tries.to_string()]);
    input::queue_chat(line(&config.chat_color_info, t.chat_info, &body));
}

/// Автопитьё долило баф.
pub fn potion_used(config: &Config, id: i32, name: &str) {
    if !config.chat_messages {
        return;
    }
    let t = lang::t();
    let body = lang::fill(t.chat_potion_used, &[&item(id, name)]);
    input::queue_chat(line(&config.chat_color_potion, t.chat_potion, &body));
}
