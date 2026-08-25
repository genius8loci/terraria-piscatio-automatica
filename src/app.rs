//! Рабочий поток: хоткеи, автомат рыбалки, синхронизация конфига и панели.
//!
//! Цикл хоткеев живёт независимо от подключения к игре — иначе неудачный
//! attach убивал бы поток вместе с возможностью выгрузить DLL.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use crate::config::{Config, FilterMode};
use crate::fishing::Fishing;
use crate::game::Game;
use crate::overlay::state::{self, Mark};
use crate::{SHOW_UI, SHUTDOWN, UNLOAD_REQUESTED, detour, input, log, overlay};

const POLL_INTERVAL: Duration = Duration::from_millis(30);
/// Чтение состояния игры заметно дороже опроса клавиш.
const TICK_INTERVAL: Duration = Duration::from_millis(120);
const STATUS_INTERVAL: Duration = Duration::from_secs(30);
/// Игра может быть ещё в сплэше — сборка Terraria появится не сразу.
const ATTACH_RETRY: Duration = Duration::from_millis(750);
const ATTACH_ATTEMPTS: u32 = 40;

/// Отслеживает нажатие клавиши по фронту, а не по удержанию.
struct KeyEdge {
    vk: i32,
    was_down: bool,
}

impl KeyEdge {
    fn new(vk: u16) -> Self {
        KeyEdge {
            vk: vk as i32,
            was_down: false,
        }
    }

    fn pressed(&mut self) -> bool {
        let down = unsafe { GetAsyncKeyState(self.vk) as u16 & 0x8000 != 0 };
        let edge = down && !self.was_down;
        self.was_down = down;
        edge
    }
}

pub fn run(dll_dir: PathBuf) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let mut config = Config::load(&dll_dir);
    push_config(&config);
    log!(
        "конфиг: фильтр={:?}, сундуки={}, автопитьё={}",
        config.filter_mode,
        config.quick_stack_when_full,
        config.auto_potions
    );
    log!("хоткеи: вверх — панель, вниз — старт/стоп, Delete — выгрузка");

    let mut key_ui = KeyEdge::new(config.hotkey_ui);
    let mut key_toggle = KeyEdge::new(config.hotkey_toggle);
    let mut key_unload = KeyEdge::new(config.hotkey_unload);

    let mut game: Option<Game> = None;
    let mut attempts: u32 = 0;
    let mut next_attach = Instant::now();
    let mut gave_up = false;

    let mut fishing = Fishing::new();
    let mut last_tick = Instant::now() - TICK_INTERVAL;
    let mut last_status = Instant::now();
    let started = Instant::now();

    if overlay::install() {
        log!("оверлей установлен, панель по стрелке вверх");
    }

    while !SHUTDOWN.load(Ordering::Relaxed) {
        // Хоткеи опрашиваем первыми и всегда: выгрузка должна работать
        // даже когда подключиться к игре не удалось.
        if key_unload.pressed() {
            log!("запрошена выгрузка");
            UNLOAD_REQUESTED.store(true, Ordering::Relaxed);
            SHUTDOWN.store(true, Ordering::Relaxed);
            break;
        }

        if key_ui.pressed() {
            let shown = !SHOW_UI.load(Ordering::Relaxed);
            SHOW_UI.store(shown, Ordering::Relaxed);
            log!(
                "панель {}",
                if shown {
                    "показана"
                } else {
                    "скрыта"
                }
            );
        }

        if key_toggle.pressed() {
            state::with(|s| {
                s.auto_fish = !s.auto_fish;
                s.dirty = true;
            });
        }

        if game.is_none() && !gave_up && Instant::now() >= next_attach {
            attempts += 1;
            let verbose = attempts == 1 || attempts % 10 == 0;
            match Game::attach(verbose) {
                Ok(attached) => {
                    log!("подключились к игре с попытки {attempts}");
                    let version = attached.version();
                    state::with(|s| {
                        s.status.connected = match &version {
                            Some(v) => format!("подключено, {v}"),
                            None => "подключено".to_string(),
                        };
                    });
                    apply_settings(&attached, &config);
                    let ready = install_detour(&attached);
                    state::with(|s| s.status.detour_ready = ready);
                    load_fishable(&attached);
                    game = Some(attached);
                }
                Err(e) => {
                    if verbose {
                        log!("попытка {attempts}: {e}");
                    }
                    if attempts >= ATTACH_ATTEMPTS {
                        gave_up = true;
                        state::with(|s| s.status.connected = "игра не найдена".to_string());
                        log!(
                            "подключиться не удалось за {ATTACH_ATTEMPTS} попыток. \
                             Хоткеи работают, выгрузка по Delete доступна"
                        );
                    }
                    next_attach = Instant::now() + ATTACH_RETRY;
                }
            }
        }

        if let Some(attached) = game.as_ref() {
            if last_tick.elapsed() >= TICK_INTERVAL {
                last_tick = Instant::now();
                fishing.tick(attached, &config);
                state::with(|s| s.stats.seconds = started.elapsed().as_secs());
            }
            if last_status.elapsed() >= STATUS_INTERVAL {
                last_status = Instant::now();
                log!(
                    "статус: {} | детур сработал {} раз, кликов {}, сбоев {}",
                    fishing.status(),
                    input::FIRED.load(Ordering::Relaxed),
                    input::CLICKS.load(Ordering::Relaxed),
                    input::FAILURES.load(Ordering::Relaxed)
                );
            }
        }

        // Переключатели из UI сохраняем в конфиг.
        if state::with(|s| std::mem::take(&mut s.dirty)).unwrap_or(false) {
            pull_config(&mut config);
            config.save(&dll_dir);
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    log!("рабочий поток остановлен");
    unsafe { CoUninitialize() };
}

/// Конфиг -> общее состояние (при старте).
fn push_config(config: &Config) {
    state::with(|s| {
        s.quick_stack = config.quick_stack_when_full;
        s.auto_potions = config.auto_potions;
        s.potions = config.potions;
        s.whitelist_mode = config.filter_mode == FilterMode::Whitelist;
        s.filter.clear();
        for id in &config.whitelist {
            s.filter.insert(*id, Mark::Allow);
        }
        for id in &config.blacklist {
            s.filter.insert(*id, Mark::Deny);
        }
    });
}

/// Общее состояние -> конфиг (после правок в UI).
fn pull_config(config: &mut Config) {
    state::with(|s| {
        config.quick_stack_when_full = s.quick_stack;
        config.auto_potions = s.auto_potions;
        config.potions = s.potions;
        config.filter_mode = if s.whitelist_mode {
            FilterMode::Whitelist
        } else {
            FilterMode::Blacklist
        };
        config.whitelist = s
            .filter
            .iter()
            .filter(|(_, m)| **m == Mark::Allow)
            .map(|(id, _)| *id)
            .collect();
        config.blacklist = s
            .filter
            .iter()
            .filter(|(_, m)| **m == Mark::Deny)
            .map(|(id, _)| *id)
            .collect();
        config.whitelist.sort_unstable();
        config.blacklist.sort_unstable();
    });
}

/// Список ловимого берём у игры и отдаём оверлею под атлас иконок.
fn load_fishable(game: &Game) {
    match game.fishable_items() {
        Ok(items) => {
            log!("ловится предметов: {}", items.len());
            // В атлас кладём ещё и зелья: без них ячейки автопитья пустые.
            let mut icons = items.clone();
            icons.extend(crate::game::POTIONS.iter().map(|(item, _, _)| *item));
            overlay::set_icon_items(icons);
            state::with(|s| s.fishable = items);
        }
        Err(e) => log!("список ловимого получить не удалось: {e}"),
    }
}

/// Ставит детур на `Player.ItemCheck`.
fn install_detour(game: &Game) -> bool {
    let address = match game.item_check_address() {
        Ok(a) => a,
        Err(e) => {
            log!("детур ItemCheck: адрес получить не удалось: {e}");
            return false;
        }
    };
    log!("детур ItemCheck: адрес 0x{address:08X}");
    log!("детур ItemCheck: первые байты {}", detour::peek(address));

    if detour::install(address) {
        log!("детур ItemCheck установлен");
        true
    } else {
        false
    }
}

fn apply_settings(game: &Game, config: &Config) {
    if !config.disable_inactive_throttle {
        return;
    }
    match game.set_inactive_throttle(false) {
        Ok(()) => match game.inactive_throttle() {
            Ok(false) => log!("ThrottleWhenInactive снят — свёрнутая игра не будет спать"),
            Ok(true) => log!("ThrottleWhenInactive записан, но остался включённым"),
            Err(e) => log!("ThrottleWhenInactive записан, перечитать не вышло: {e}"),
        },
        Err(e) => log!("не удалось снять троттлинг: {e}"),
    }
}
