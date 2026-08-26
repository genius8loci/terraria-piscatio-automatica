//! Рабочий поток: хоткеи, автомат рыбалки, синхронизация конфига и панели.
//!
//! Цикл хоткеев живёт независимо от подключения к игре — иначе неудачный
//! attach убивал бы поток вместе с возможностью выгрузить DLL.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::config::{Config, FilterMode};
use crate::fishing::Fishing;
use crate::game::Game;
use crate::overlay::state::{self, Mark};
use crate::{SHOW_UI, SHUTDOWN, UNLOAD_REQUESTED, crash, detour, input, lang, log, overlay};

const POLL_INTERVAL: Duration = Duration::from_millis(30);
/// Чтение состояния игры заметно дороже опроса клавиш.
const TICK_INTERVAL: Duration = Duration::from_millis(120);
const STATUS_INTERVAL: Duration = Duration::from_secs(30);
/// Как часто перечитывать язык игры. Игрок меняет его прямо в настройках,
/// не выходя из мира, а вызов стоит одного обращения к рефлексии.
const LANG_INTERVAL: Duration = Duration::from_secs(2);
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

    /// Нажатие по фронту. `active` — окно игры сейчас впереди; если нет,
    /// фронт всё равно снимается, но наружу не отдаётся: иначе после
    /// возвращения в игру сработало бы разом всё, что нажималось мимо.
    fn pressed(&mut self, active: bool) -> bool {
        let down = unsafe { GetAsyncKeyState(self.vk) as u16 & 0x8000 != 0 };
        let edge = down && !self.was_down;
        self.was_down = down;
        edge && active
    }
}

/// Окно игры сейчас впереди.
///
/// `GetAsyncKeyState` слышит клавиатуру всегда, хоть в браузере, хоть при
/// свёрнутой игре. Без этой проверки набранное в другом окне долетало до
/// хоткеев: стрелки дёргали панель, а `Delete` выгружал DLL.
fn game_focused() -> bool {
    unsafe {
        let window = GetForegroundWindow();
        if window.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&mut pid));
        pid != 0 && pid == GetCurrentProcessId()
    }
}

pub fn run(dll_dir: PathBuf) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    // Ловушке падений нужно знать, какой поток тут рабочий: иначе по строке
    // падения не понять, кто именно упал.
    crash::mark_worker_thread();

    // Первой строкой — кто именно запустился. В логе накапливаются сессии
    // разных сборок, и без версии непонятно, к какой относится запись.
    log!(
        "{} v{}, DLL из {}",
        env!("CARGO_PKG_DESCRIPTION"),
        env!("CARGO_PKG_VERSION"),
        dll_dir.display()
    );

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
    let mut last_lang = Instant::now();

    // Стрелка сворачивания видна с самого начала: без неё панель нечем
    // открыть мышью, а хоткей знает не всякий.
    SHOW_UI.store(true, Ordering::Relaxed);
    if overlay::install() {
        log!("оверлей установлен, стрелка вверх раскрывает и сворачивает панель");
    }

    while !SHUTDOWN.load(Ordering::Relaxed) {
        // Хоткеи слушаем только когда игра впереди и когда не идёт набор
        // в строке поиска. Фронт нажатия снимаем в любом случае, иначе
        // после возвращения в игру сработало бы разом всё нажатое мимо.
        let active = game_focused() && !overlay::is_typing();

        if key_unload.pressed(active) {
            log!("запрошена выгрузка");
            UNLOAD_REQUESTED.store(true, Ordering::Relaxed);
            SHUTDOWN.store(true, Ordering::Relaxed);
            break;
        }

        if key_ui.pressed(active) {
            overlay::toggle_expanded();
        }

        if key_toggle.pressed(active) {
            state::with(|s| {
                s.auto_fish = !s.auto_fish;
                s.dirty = true;
            });
        }

        if game.is_none() && !gave_up && Instant::now() >= next_attach {
            attempts += 1;
            let verbose = attempts == 1 || attempts.is_multiple_of(10);
            match Game::attach(verbose) {
                Ok(mut attached) => {
                    log!("подключились к игре с попытки {attempts}");
                    let version = attached.version();
                    state::with(|s| {
                        s.status.connected = match &version {
                            Some(v) => format!("подключено, {v}"),
                            None => "подключено".to_string(),
                        };
                    });
                    // Язык панели — язык игры: русский, если у игрока русский,
                    // иначе английский. Спрашиваем один раз, на подключении.
                    let culture = attached.culture_id();
                    lang::set_russian(culture == Some(lang::RUSSIAN_ID));
                    log!(
                        "язык панели: {} (культура игры {})",
                        if lang::is_russian() {
                            "русский"
                        } else {
                            "английский"
                        },
                        match culture {
                            Some(id) => id.to_string(),
                            None => "неизвестна".to_string(),
                        }
                    );
                    apply_settings(&attached, &config);
                    let ready = install_detour(&attached);
                    state::with(|s| s.status.detour_ready = ready);
                    if config.cursor_detour {
                        install_cursor_detour(&attached);
                    } else {
                        log!("детур DrawCursor отключён в конфиге");
                    }
                    load_fishable(&mut attached);
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
            }
            // Язык игрок может переключить прямо в игре, не выходя из мира.
            // Перечитываем культуру время от времени: вызов дешёвый, а иначе
            // панель осталась бы на языке, который был при инжекте.
            if last_lang.elapsed() >= LANG_INTERVAL {
                last_lang = Instant::now();
                let step = crash::Step::worker(crash::STEP_LANG);
                let russian = attached.culture_id() == Some(lang::RUSSIAN_ID);
                drop(step);
                if russian != lang::is_russian() {
                    lang::set_russian(russian);
                    log!(
                        "язык панели переключён на {}",
                        if russian {
                            "русский"
                        } else {
                            "английский"
                        }
                    );
                }
            }
            if last_status.elapsed() >= STATUS_INTERVAL {
                last_status = Instant::now();
                log!(
                    "статус: {} | детур сработал {} раз, кликов {}, сбоев {}, раскладок {}, панель из DrawCursor {} раз",
                    fishing.status(),
                    input::FIRED.load(Ordering::Relaxed),
                    input::CLICKS.load(Ordering::Relaxed),
                    input::FAILURES.load(Ordering::Relaxed),
                    input::STACKS.load(Ordering::Relaxed),
                    overlay::CURSOR_DRAWS.load(Ordering::Relaxed)
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
        s.pull_enemy_spawns = config.pull_enemy_spawns;
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
        config.pull_enemy_spawns = s.pull_enemy_spawns;
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
fn load_fishable(game: &mut Game) {
    let _step = crash::Step::worker(crash::STEP_NAMES);
    match game.fishable_items() {
        Ok(items) => {
            log!("ловится предметов: {}", items.len());
            // В атлас кладём ещё и зелья: без них ячейки автопитья пустые.
            let mut icons = items.clone();
            icons.extend(crate::game::POTIONS.iter().map(|(item, _, _)| *item));
            // У анимированных предметов в файле лежит лента кадров: без числа
            // кадров в ячейку попала бы вся лента (так вылезала Joja Cola).
            let icons: Vec<(i32, u32)> = icons
                .into_iter()
                .map(|id| (id, game.item_frames(id).unwrap_or(1)))
                .collect();
            let animated = icons.iter().filter(|(_, frames)| *frames > 1).count();
            if animated > 0 {
                log!("анимированных иконок: {animated}");
            }
            overlay::set_icon_items(icons);

            // Имена нужны поиску в фильтре. Спрашиваем их один раз здесь,
            // на рабочем потоке: на потоке рендера столько вызовов в CLR
            // за кадр делать нельзя.
            let mut names: Vec<state::ItemFacts> = Vec::with_capacity(items.len());
            for id in &items {
                if let Some((name, quest)) = game.item_facts(*id) {
                    names.push(state::ItemFacts {
                        id: *id,
                        search: name.to_lowercase(),
                        display: name,
                        quest,
                    });
                }
            }
            // Зелья в `FishDropsDB` не попадают, а их имена нужны чату.
            for (item, _, _) in crate::game::POTIONS.iter() {
                if let Some((name, quest)) = game.item_facts(*item) {
                    names.push(state::ItemFacts {
                        id: *item,
                        search: name.to_lowercase(),
                        display: name,
                        quest,
                    });
                }
            }
            log!("имён предметов получено: {}", names.len());

            state::with(|s| {
                s.fishable = items;
                s.names = names;
            });
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

/// Детур на `Main.DrawCursor`: с ним панель ложится под курсор игры.
/// Не встал — не беда, оверлей нарисует курсор сам из `Present`.
fn install_cursor_detour(game: &Game) {
    match game.draw_cursor_address() {
        Ok(address) => {
            log!("детур DrawCursor: адрес 0x{address:08X}");
            log!("детур DrawCursor: первые байты {}", detour::peek(address));
            if detour::install_cursor(address) {
                log!("детур DrawCursor установлен");
            }
        }
        Err(e) => log!("детур DrawCursor: адрес получить не удалось: {e}"),
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
