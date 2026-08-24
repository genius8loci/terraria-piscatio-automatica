//! Рабочий поток: хоткеи и наблюдение за состоянием рыбалки.
//!
//! На этом этапе поток только читает состояние и пишет в лог — это проверка
//! того, что рефлексия действительно видит живую игру. Управление забросом
//! переедет в детур `Player.ItemCheck`, потому что при свёрнутом окне
//! реальный ввод игрой игнорируется.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use crate::config::Config;
use crate::game::Game;
use crate::{SHOW_UI, SHUTDOWN, UNLOAD_REQUESTED, log};

const POLL_INTERVAL: Duration = Duration::from_millis(30);
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

    let game = match attach_with_retry() {
        Some(game) => game,
        None => {
            log!("не удалось подцепиться к игре, поток завершается");
            unsafe { CoUninitialize() };
            return;
        }
    };

    if let Some(version) = game.version() {
        log!("версия игры: {version}");
    }

    if config.disable_inactive_throttle {
        match game.set_inactive_throttle(false) {
            Ok(()) => log!(
                "ThrottleWhenInactive выключен (сейчас {:?}) — свёрнутая игра не будет спать",
                game.inactive_throttle()
            ),
            Err(e) => log!("не удалось снять троттлинг: {e}"),
        }
    }

    let mut key_ui = KeyEdge::new(config.hotkey_ui);
    let mut key_toggle = KeyEdge::new(config.hotkey_toggle);
    let mut key_unload = KeyEdge::new(config.hotkey_unload);

    let mut enabled = false;
    let mut last_status = Instant::now() - STATUS_INTERVAL;
    let mut last_bite = 0i32;

    log!("готов. Insert — UI, End — старт/стоп, Delete — выгрузка");

    while !SHUTDOWN.load(Ordering::Relaxed) {
        if key_unload.pressed() {
            log!("запрошена выгрузка");
            UNLOAD_REQUESTED.store(true, Ordering::Relaxed);
            SHUTDOWN.store(true, Ordering::Relaxed);
            break;
        }

        if key_ui.pressed() {
            let shown = !SHOW_UI.load(Ordering::Relaxed);
            SHOW_UI.store(shown, Ordering::Relaxed);
            log!("UI {}", if shown { "показан" } else { "скрыт" });
        }

        if key_toggle.pressed() {
            enabled = !enabled;
            log!("рыбалка {}", if enabled { "включена" } else { "выключена" });
            config.save(&dll_dir);
        }

        // Пока автомат заброса не подключён, наблюдаем и подтверждаем,
        // что состояние поплавка читается корректно.
        match game.find_bobber() {
            Ok(Some(bobber)) => {
                if bobber.has_bite() {
                    let rolled = bobber.rolled();
                    if rolled != last_bite {
                        last_bite = rolled;
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
                    last_bite = 0;
                }

                if last_status.elapsed() >= STATUS_INTERVAL {
                    last_status = Instant::now();
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
                if last_status.elapsed() >= STATUS_INTERVAL {
                    last_status = Instant::now();
                    report_idle(&game);
                }
            }
            Err(e) => {
                log!("чтение поплавка не удалось: {e}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    log!("рабочий поток остановлен");
    unsafe { CoUninitialize() };
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

fn attach_with_retry() -> Option<Game> {
    for attempt in 1..=ATTACH_ATTEMPTS {
        if SHUTDOWN.load(Ordering::Relaxed) {
            return None;
        }
        match Game::attach() {
            Ok(game) => {
                log!("подцепились к игре с попытки {attempt}");
                return Some(game);
            }
            Err(e) => {
                if attempt == 1 || attempt % 10 == 0 {
                    log!("попытка {attempt}: {e}");
                }
                std::thread::sleep(ATTACH_RETRY);
            }
        }
    }
    None
}
