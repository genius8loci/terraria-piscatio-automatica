//! Рабочий поток: хоткеи, автомат рыбалки и панель состояния.
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
use crate::{SHOW_UI, SHUTDOWN, UNLOAD_REQUESTED, detour, input, log, overlay};

const POLL_INTERVAL: Duration = Duration::from_millis(30);
/// Чтение состояния игры заметно дороже опроса клавиш.
const TICK_INTERVAL: Duration = Duration::from_millis(120);
const STATUS_INTERVAL: Duration = Duration::from_secs(10);
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

    let config = Config::load(&dll_dir);
    log!(
        "конфиг: фильтр={:?}, сундуки={}, снятие троттлинга={}",
        config.filter_mode,
        config.quick_stack_when_full,
        config.disable_inactive_throttle
    );
    log!("хоткеи: вверх — панель, вниз — старт/стоп, Delete — выгрузка");

    let mut key_ui = KeyEdge::new(config.hotkey_ui);
    let mut key_toggle = KeyEdge::new(config.hotkey_toggle);
    let mut key_unload = KeyEdge::new(config.hotkey_unload);

    let mut game: Option<Game> = None;
    let mut attempts: u32 = 0;
    let mut next_attach = Instant::now();
    let mut gave_up = false;
    let mut state_line = "ищу игру".to_string();
    let mut detour_line = "не установлен".to_string();

    let mut fishing = Fishing::new();
    let mut last_tick = Instant::now() - TICK_INTERVAL;
    let mut last_status = Instant::now() - STATUS_INTERVAL;

    if overlay::install() {
        log!("оверлей установлен, панель по стрелке вверх");
    }
    publish(&config, &fishing, &state_line, &detour_line);

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
            fishing.toggle();
            publish(&config, &fishing, &state_line, &detour_line);
        }

        if game.is_none() && !gave_up && Instant::now() >= next_attach {
            attempts += 1;
            let verbose = attempts == 1 || attempts % 10 == 0;
            match Game::attach(verbose) {
                Ok(attached) => {
                    log!("подключились к игре с попытки {attempts}");
                    state_line = match attached.version() {
                        Some(v) => format!("подключено, {v}"),
                        None => "подключено".to_string(),
                    };
                    apply_settings(&attached, &config);
                    detour_line = install_detour(&attached);
                    game = Some(attached);
                }
                Err(e) => {
                    if verbose {
                        log!("попытка {attempts}: {e}");
                    }
                    if attempts >= ATTACH_ATTEMPTS {
                        gave_up = true;
                        state_line = "игра не найдена".to_string();
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
                publish(&config, &fishing, &state_line, &detour_line);
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

        std::thread::sleep(POLL_INTERVAL);
    }

    log!("рабочий поток остановлен");
    unsafe { CoUninitialize() };
}

/// Ставит детур на `Player.ItemCheck` и возвращает строку для панели.
fn install_detour(game: &Game) -> String {
    let address = match game.item_check_address() {
        Ok(a) => a,
        Err(e) => {
            log!("детур ItemCheck: адрес получить не удалось: {e}");
            return "адрес не получен".to_string();
        }
    };
    log!("детур ItemCheck: адрес 0x{address:08X}");
    log!("детур ItemCheck: первые байты {}", detour::peek(address));

    if detour::install(address) {
        log!("детур ItemCheck установлен");
        "установлен".to_string()
    } else {
        "не встал".to_string()
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

fn publish(config: &Config, fishing: &Fishing, state: &str, detour_line: &str) {
    let filter = match config.filter_mode {
        FilterMode::Blacklist => format!("чёрный список ({})", config.blacklist.len()),
        FilterMode::Whitelist => format!("белый список ({})", config.whitelist.len()),
    };
    let pair = |label: &str, value: String| (label.to_string(), value);
    overlay::set_lines(vec![
        pair("состояние", state.to_string()),
        pair("детур", detour_line.to_string()),
        pair("рыбалка", fishing.status()),
        pair("фильтр", filter),
        pair(
            "сундуки",
            if config.quick_stack_when_full {
                "разложить при заполнении"
            } else {
                "не трогать"
            }
            .to_string(),
        ),
        pair("поплавок", fishing.bobber_line.clone()),
        pair("запасы", fishing.stock_line.clone()),
        (String::new(), String::new()),
        (
            String::new(),
            "вверх — панель, вниз — старт/стоп, Delete — выгрузка".to_string(),
        ),
    ]);
}
