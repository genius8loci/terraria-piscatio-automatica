//! Рабочий поток: хоткеи и наблюдение за состоянием рыбалки.
//!
//! Цикл хоткеев живёт независимо от подключения к игре — иначе неудачный
//! attach убивал бы поток вместе с возможностью выгрузить DLL.
//!
//! На этом этапе поток только читает состояние и пишет в лог. Управление
//! забросом переедет в детур `Player.ItemCheck`, потому что при свёрнутом
//! окне реальный ввод игрой игнорируется.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use crate::config::Config;
use crate::game::Game;
use crate::{SHOW_UI, SHUTDOWN, UNLOAD_REQUESTED, log};

const POLL_INTERVAL: Duration = Duration::from_millis(30);
/// Полный перебор снарядов недёшев, поэтому состояние читаем реже опроса клавиш.
const OBSERVE_INTERVAL: Duration = Duration::from_millis(250);
const STATUS_INTERVAL: Duration = Duration::from_secs(5);
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

struct Watcher {
    last_status: Instant,
    last_observe: Instant,
    last_bite: i32,
    bobber_hint: Option<i32>,
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
    log!("хоткеи: вверх — UI, вниз — старт/стоп, Delete — выгрузка");

    let mut key_ui = KeyEdge::new(config.hotkey_ui);
    let mut key_toggle = KeyEdge::new(config.hotkey_toggle);
    let mut key_unload = KeyEdge::new(config.hotkey_unload);

    let mut game: Option<Game> = None;
    let mut attempts: u32 = 0;
    let mut next_attach = Instant::now();
    let mut gave_up = false;
    let mut enabled = false;

    let mut watcher = Watcher {
        last_status: Instant::now() - STATUS_INTERVAL,
        last_observe: Instant::now() - OBSERVE_INTERVAL,
        last_bite: 0,
        bobber_hint: None,
    };

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
                "UI {}",
                if shown {
                    "показан"
                } else {
                    "скрыт"
                }
            );
        }

        if key_toggle.pressed() {
            enabled = !enabled;
            log!(
                "рыбалка {}",
                if enabled {
                    "включена"
                } else {
                    "выключена"
                }
            );
        }

        if game.is_none() && !gave_up && Instant::now() >= next_attach {
            attempts += 1;
            let verbose = attempts == 1 || attempts % 10 == 0;
            match Game::attach(verbose) {
                Ok(attached) => {
                    log!("подключились к игре с попытки {attempts}");
                    if let Some(version) = attached.version() {
                        log!("версия игры: {version}");
                    }
                    apply_settings(&attached, &config);
                    game = Some(attached);
                }
                Err(e) => {
                    if verbose {
                        log!("попытка {attempts}: {e}");
                    }
                    if attempts >= ATTACH_ATTEMPTS {
                        gave_up = true;
                        log!(
                            "подключиться не удалось за {ATTACH_ATTEMPTS} попыток. \
                             Хоткеи продолжают работать, выгрузка по Delete доступна"
                        );
                    }
                    next_attach = Instant::now() + ATTACH_RETRY;
                }
            }
        }

        if let Some(attached) = game.as_ref() {
            if watcher.last_observe.elapsed() >= OBSERVE_INTERVAL {
                watcher.last_observe = Instant::now();
                observe(attached, &config, &mut watcher);
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    log!("рабочий поток остановлен");
    unsafe { CoUninitialize() };
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

/// Пока автомат заброса не подключён, наблюдаем и подтверждаем,
/// что состояние поплавка читается корректно.
fn observe(game: &Game, config: &Config, watcher: &mut Watcher) {
    match game.find_bobber(watcher.bobber_hint) {
        Ok(Some(bobber)) => {
            watcher.bobber_hint = Some(bobber.index);

            if bobber.has_bite() {
                let rolled = bobber.rolled();
                if rolled != watcher.last_bite {
                    watcher.last_bite = rolled;
                    let decision = if config.should_pull(rolled) {
                        "подсекаем"
                    } else {
                        "пропускаем"
                    };
                    log!(
                        "поклёвка: localAI[1]={rolled} ({}), ai[1]={:.0} -> {decision}",
                        if rolled > 0 { "предмет" } else { "NPC" },
                        bobber.ai1
                    );
                }
            } else {
                watcher.last_bite = 0;
            }

            if watcher.last_status.elapsed() >= STATUS_INTERVAL {
                watcher.last_status = Instant::now();
                log!(
                    "поплавок #{}: ai[0]={:.0} ai[1]={:.0} localAI[1]={:.0}{}",
                    bobber.index,
                    bobber.ai0,
                    bobber.ai1,
                    bobber.local_ai1,
                    if bobber.is_reeling() {
                        " (тянется)"
                    } else {
                        ""
                    }
                );
            }
        }
        Ok(None) => {
            watcher.bobber_hint = None;
            if watcher.last_status.elapsed() >= STATUS_INTERVAL {
                watcher.last_status = Instant::now();
                report_idle(game);
            }
        }
        Err(e) => {
            log!("чтение поплавка не удалось: {e}");
            watcher.last_observe = Instant::now() + Duration::from_secs(1);
        }
    }
}

fn report_idle(game: &Game) {
    let Ok(Some(player)) = game.local_player() else {
        log!("поплавка нет, локальный игрок пока недоступен");
        return;
    };
    let bait = game.bait_total(&player).unwrap_or(-1);
    let free = game.free_slots(&player).unwrap_or(-1);
    log!("поплавка нет. наживка={bait}, свободных слотов={free}");
}
