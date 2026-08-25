//! Автомат рыбалки.
//!
//! Решения принимаются здесь, на рабочем потоке, а нажатие применяет детур
//! `Player.ItemCheck` (см. `input`). Точка заброса запоминается по первому
//! ручному забросу игрока и дальше переиспользуется.

use std::time::{Duration, Instant};

use crate::config::Config;
use crate::detour;
use crate::game::Game;
use crate::input;
use crate::log;

/// Пауза после подсечки перед повторным забросом.
const AFTER_PULL: u64 = 350;
/// Пауза после заброса, пока поплавок летит.
const AFTER_CAST: u64 = 700;
/// Сколько ждать применения нажатия, прежде чем считать его потерянным.
const CLICK_TIMEOUT: Duration = Duration::from_millis(1200);
/// Как часто перечитывать наживку и слоты для панели.
const STOCK_INTERVAL: Duration = Duration::from_secs(2);

pub struct Fishing {
    pub enabled: bool,
    /// Экранные координаты первого заброса.
    aim: Option<(i32, i32)>,
    next_action: Instant,
    hint: Option<i32>,
    last_bite: i32,
    stopped: Option<String>,
    rng: u32,
    /// Когда поставили нажатие в очередь — чтобы поймать потерянное.
    click_sent: Option<Instant>,
    last_stock: Instant,
    warned_no_detour: bool,

    pub casts: u32,
    pub pulls: u32,
    pub skips: u32,
    pub bobber_line: String,
    pub stock_line: String,
}

impl Fishing {
    pub fn new() -> Self {
        Fishing {
            enabled: false,
            aim: None,
            next_action: Instant::now(),
            hint: None,
            last_bite: 0,
            stopped: None,
            rng: 0x9E37_79B9,
            click_sent: None,
            last_stock: Instant::now() - STOCK_INTERVAL,
            warned_no_detour: false,
            casts: 0,
            pulls: 0,
            skips: 0,
            bobber_line: "не заброшен".to_string(),
            stock_line: "неизвестно".to_string(),
        }
    }

    /// Строка состояния для панели.
    pub fn status(&self) -> String {
        if let Some(reason) = &self.stopped {
            return format!("стоп: {reason}");
        }
        if !self.enabled {
            return "выключена".to_string();
        }
        if !detour::is_active() {
            return "детур не стоит — нажимать некому".to_string();
        }
        if self.aim.is_none() {
            return "жду первый заброс вручную".to_string();
        }
        format!(
            "работает | забросов {} уловов {} пропущено {}",
            self.casts, self.pulls, self.skips
        )
    }

    pub fn toggle(&mut self) {
        self.enabled = !self.enabled;
        if self.enabled {
            self.stopped = None;
            self.next_action = Instant::now();
            log!(
                "рыбалка включена. Точка заброса: {}",
                match self.aim {
                    Some((x, y)) => format!("{x},{y}"),
                    None => "не задана, сделай первый заброс вручную".to_string(),
                }
            );
        } else {
            log!("рыбалка выключена");
            // Следующий ручной заброс задаст новую точку: обычно выключают
            // именно чтобы переставить поплавок.
            self.forget_aim();
        }
    }

    /// Сбрасывает запомненную точку — следующий ручной заброс задаст новую.
    pub fn forget_aim(&mut self) {
        self.aim = None;
        log!("точка заброса сброшена");
    }

    fn jitter(&mut self, config: &Config, base: u64) -> Duration {
        // Xorshift: своего генератора достаточно, тянуть зависимость незачем.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        let span = config
            .jitter_max_ms
            .saturating_sub(config.jitter_min_ms)
            .max(1);
        let extra = config.jitter_min_ms + (self.rng as u64) % span;
        Duration::from_millis(base + extra)
    }

    /// Ставит нажатие в очередь. Без детура применять его некому.
    fn click(&mut self, aim: Option<(i32, i32)>) -> bool {
        if !detour::is_active() {
            if !self.warned_no_detour {
                self.warned_no_detour = true;
                log!("рыбалка: детур не стоит, нажать некому — автомат простаивает");
            }
            return false;
        }
        input::request_click(aim);
        self.click_sent = Some(Instant::now());
        true
    }

    /// Потерянное нажатие снимаем сами, иначе `busy()` навсегда останется
    /// истинным и автомат встанет.
    fn drop_stale_click(&mut self) {
        let Some(sent) = self.click_sent else {
            return;
        };
        if !input::busy() {
            self.click_sent = None;
            return;
        }
        if sent.elapsed() > CLICK_TIMEOUT {
            input::cancel();
            self.click_sent = None;
            log!("ввод: нажатие не применилось за {CLICK_TIMEOUT:?}, снял команду");
        }
    }

    fn refresh_stock(&mut self, game: &Game) -> (i32, i32) {
        let Ok(Some(player)) = game.local_player() else {
            self.stock_line = "игрок недоступен".to_string();
            return (-1, -1);
        };
        let bait = game.bait_total(&player).unwrap_or(-1);
        let free = game.free_slots(&player).unwrap_or(-1);
        self.stock_line = if bait == 0 {
            format!("НАЖИВКИ НЕТ, слотов {free}")
        } else {
            format!("наживка {bait}, слотов {free}")
        };
        (bait, free)
    }

    pub fn tick(&mut self, game: &Game, config: &Config) {
        self.drop_stale_click();

        // Запасы показываем всегда, а не только перед забросом.
        if self.last_stock.elapsed() >= STOCK_INTERVAL {
            self.last_stock = Instant::now();
            self.refresh_stock(game);
        }

        match game.find_bobber(self.hint) {
            Ok(Some(bobber)) => {
                self.hint = Some(bobber.index);
                self.remember_aim(game);

                if bobber.has_bite() {
                    self.bobber_line = format!(
                        "#{} КЛЮЁТ {} {}",
                        bobber.index,
                        if bobber.rolled() > 0 {
                            "предмет"
                        } else {
                            "NPC"
                        },
                        bobber.rolled()
                    );
                    self.on_bite(config, bobber.rolled());
                } else {
                    self.last_bite = 0;
                    self.bobber_line = if bobber.is_reeling() {
                        format!("#{} тянется", bobber.index)
                    } else {
                        format!("#{} ждём, заряд {:.0}/660", bobber.index, bobber.local_ai1)
                    };
                }
            }
            Ok(None) => {
                self.hint = None;
                self.bobber_line = "не заброшен".to_string();
                self.on_idle(game, config);
            }
            Err(e) => {
                log!("рыбалка: чтение поплавка не удалось: {e}");
            }
        }
    }

    /// Первый заброс делает игрок — оттуда и берём точку прицела.
    fn remember_aim(&mut self, game: &Game) {
        if self.aim.is_some() {
            return;
        }
        match game.mouse() {
            Ok((x, y)) if x >= 0 && y >= 0 => {
                self.aim = Some((x, y));
                log!("точка заброса запомнена: {x},{y}");
            }
            Ok(_) => {}
            Err(e) => log!("рыбалка: курсор прочитать не удалось: {e}"),
        }
    }

    fn on_bite(&mut self, config: &Config, rolled: i32) {
        if rolled == self.last_bite {
            return;
        }
        self.last_bite = rolled;

        if !config.should_pull(rolled) {
            self.skips += 1;
            log!("пропуск: улов {rolled} не проходит фильтр, наживка не тратится");
            return;
        }
        if !self.enabled {
            log!("поклёвка: улов {rolled} прошёл бы фильтр, но рыбалка выключена");
            return;
        }
        if input::busy() || Instant::now() < self.next_action {
            return;
        }

        if !self.click(None) {
            return;
        }
        self.pulls += 1;
        log!("подсечка #{}: улов {rolled}", self.pulls);
        let pause = self.jitter(config, AFTER_PULL);
        self.next_action = Instant::now() + pause;
    }

    fn on_idle(&mut self, game: &Game, config: &Config) {
        if !self.enabled || self.stopped.is_some() {
            return;
        }
        let Some(aim) = self.aim else {
            return;
        };
        if input::busy() || Instant::now() < self.next_action {
            return;
        }

        // Запасы перечитываем прямо перед забросом.
        let (bait, free) = self.refresh_stock(game);
        self.last_stock = Instant::now();
        if bait < 0 {
            return;
        }

        if bait == 0 {
            self.stopped = Some("наживка кончилась".to_string());
            log!("рыбалка остановлена: наживка кончилась");
            return;
        }
        if free == 0 && config.quick_stack_when_full {
            if let Ok(Some(player)) = game.local_player() {
                match game.quick_stack_to_nearby_chests(&player) {
                    Ok(()) => log!("инвентарь полон — разложил по ближайшим сундукам"),
                    Err(e) => log!("разложить по сундукам не удалось: {e}"),
                }
            }
        }

        if !self.click(Some(aim)) {
            return;
        }
        self.casts += 1;
        log!("заброс #{} в точку {},{}", self.casts, aim.0, aim.1);
        let pause = self.jitter(config, AFTER_CAST);
        self.next_action = Instant::now() + pause;
    }
}
