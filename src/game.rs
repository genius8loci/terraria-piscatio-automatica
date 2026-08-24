//! Типизированный доступ к состоянию Terraria 1.4.5.6.
//!
//! Имена полей и семантика проверены по декомпиляции конкретной версии,
//! см. `docs/research-1.4.5.6.md`.

use windows::Win32::System::Com::IDispatch;
use windows::core::Result;

use crate::clr::{Clr, Field, Var, array_get, call, get_type};

/// Размер `Main.projectile` в 1.4.5.6.
const MAX_PROJECTILES: i32 = 1001;
/// Размер `Player.inventory`; слоты 0..49 — основная сетка.
const INVENTORY_MAIN_SLOTS: i32 = 50;

/// Снимок состояния поплавка.
#[derive(Debug, Clone, Copy)]
pub struct Bobber {
    pub index: i32,
    /// `ai[0]`: 0 — работает, >= 1 — подтягивается к игроку.
    pub ai0: f32,
    /// `ai[1]`: 0 — простой, < 0 — активная поклёвка (окно подсечки).
    pub ai1: f32,
    /// `localAI[1]`: при поклёвке — id предмета (> 0) либо минус id NPC (< 0).
    /// В простое — счётчик накопления поклёвки.
    pub local_ai1: f32,
}

impl Bobber {
    /// Клюёт прямо сейчас, и улов уже прокатан.
    pub fn has_bite(&self) -> bool {
        self.ai1 < 0.0 && self.local_ai1 != 0.0
    }

    /// Что именно клюнуло. Осмысленно только при `has_bite()`.
    pub fn rolled(&self) -> i32 {
        self.local_ai1 as i32
    }

    /// Поплавок уже тянется к игроку — новый заброс невозможен.
    pub fn is_reeling(&self) -> bool {
        self.ai0 >= 1.0
    }
}

pub struct Game {
    _clr: Clr,
    main: IDispatch,

    f_my_player: Field,
    f_player: Field,
    f_projectile: Field,
    f_throttle: Field,

    pr_active: Field,
    pr_bobber: Field,
    pr_owner: Field,
    pr_ai: Field,
    pr_local_ai: Field,

    pl_inventory: Field,
    pl_control_use_item: Field,

    it_type: Field,
    it_stack: Field,
    it_bait: Field,
}

impl Game {
    pub fn attach(verbose: bool) -> Result<Game> {
        let clr = Clr::attach(verbose)?;

        let assembly = clr.assembly("Terraria")?;
        crate::log!("сборка Terraria найдена");

        let main = get_type(&assembly, "Terraria.Main")?;
        let projectile = get_type(&assembly, "Terraria.Projectile")?;
        let player = get_type(&assembly, "Terraria.Player")?;
        let item = get_type(&assembly, "Terraria.Item")?;

        let game = Game {
            f_my_player: Field::resolve(&main, "myPlayer")?,
            f_player: Field::resolve(&main, "player")?,
            f_projectile: Field::resolve(&main, "projectile")?,
            f_throttle: Field::resolve(&main, "ThrottleWhenInactive")?,

            pr_active: Field::resolve(&projectile, "active")?,
            pr_bobber: Field::resolve(&projectile, "bobber")?,
            pr_owner: Field::resolve(&projectile, "owner")?,
            pr_ai: Field::resolve(&projectile, "ai")?,
            pr_local_ai: Field::resolve(&projectile, "localAI")?,

            pl_inventory: Field::resolve(&player, "inventory")?,
            pl_control_use_item: Field::resolve(&player, "controlUseItem")?,

            it_type: Field::resolve(&item, "type")?,
            it_stack: Field::resolve(&item, "stack")?,
            it_bait: Field::resolve(&item, "bait")?,

            main,
            _clr: clr,
        };
        crate::log!("все поля разрешены");
        Ok(game)
    }

    pub fn my_player(&self) -> Result<i32> {
        Ok(self.f_my_player.get_static()?.as_int().unwrap_or(-1))
    }

    /// `Main.player[Main.myPlayer]`.
    pub fn local_player(&self) -> Result<Option<IDispatch>> {
        let index = self.my_player()?;
        if index < 0 {
            return Ok(None);
        }
        let Some(players) = self.f_player.get_static()?.as_object() else {
            return Ok(None);
        };
        Ok(array_get(&players, index)?.as_object())
    }

    /// Снимает троттлинг при потере фокуса: без этого свёрнутая игра
    /// спит по 20 мс на кадр и рыбалка идёт втрое медленнее.
    pub fn set_inactive_throttle(&self, enabled: bool) -> Result<()> {
        self.f_throttle.set_static(Var::boolean(enabled))
    }

    pub fn inactive_throttle(&self) -> Result<bool> {
        Ok(self.f_throttle.get_static()?.as_bool().unwrap_or(true))
    }

    /// Ищет поплавок локального игрока. У игрока он всегда один —
    /// наличие поплавка запрещает новый заброс (см. research-док).
    pub fn find_bobber(&self) -> Result<Option<Bobber>> {
        let me = self.my_player()?;
        if me < 0 {
            return Ok(None);
        }
        let Some(projectiles) = self.f_projectile.get_static()?.as_object() else {
            return Ok(None);
        };

        for i in 0..MAX_PROJECTILES {
            let Some(projectile) = array_get(&projectiles, i)?.as_object() else {
                continue;
            };
            if !self.pr_active.get(&projectile)?.as_bool().unwrap_or(false) {
                continue;
            }
            if !self.pr_bobber.get(&projectile)?.as_bool().unwrap_or(false) {
                continue;
            }
            if self.pr_owner.get(&projectile)?.as_int().unwrap_or(-1) != me {
                continue;
            }

            let ai = self.pr_ai.get(&projectile)?.as_object();
            let local_ai = self.pr_local_ai.get(&projectile)?.as_object();
            let (Some(ai), Some(local_ai)) = (ai, local_ai) else {
                continue;
            };

            return Ok(Some(Bobber {
                index: i,
                ai0: array_get(&ai, 0)?.as_float().unwrap_or(0.0),
                ai1: array_get(&ai, 1)?.as_float().unwrap_or(0.0),
                local_ai1: array_get(&local_ai, 1)?.as_float().unwrap_or(0.0),
            }));
        }
        Ok(None)
    }

    /// Суммарный стак наживки в инвентаре. Логика повторяет
    /// `Player.Fishing_GetBait`: предмет считается наживкой при `bait > 0`.
    pub fn bait_total(&self, player: &IDispatch) -> Result<i32> {
        let Some(inventory) = self.pl_inventory.get(player)?.as_object() else {
            return Ok(0);
        };
        let mut total = 0;
        for i in 0..INVENTORY_MAIN_SLOTS {
            let Some(item) = array_get(&inventory, i)?.as_object() else {
                continue;
            };
            let stack = self.it_stack.get(&item)?.as_int().unwrap_or(0);
            if stack <= 0 {
                continue;
            }
            if self.it_bait.get(&item)?.as_int().unwrap_or(0) > 0 {
                total += stack;
            }
        }
        Ok(total)
    }

    /// Свободные слоты основной сетки инвентаря.
    pub fn free_slots(&self, player: &IDispatch) -> Result<i32> {
        let Some(inventory) = self.pl_inventory.get(player)?.as_object() else {
            return Ok(0);
        };
        let mut free = 0;
        for i in 0..INVENTORY_MAIN_SLOTS {
            let Some(item) = array_get(&inventory, i)?.as_object() else {
                free += 1;
                continue;
            };
            let empty = self.it_type.get(&item)?.as_int().unwrap_or(0) == 0
                || self.it_stack.get(&item)?.as_int().unwrap_or(0) == 0;
            if empty {
                free += 1;
            }
        }
        Ok(free)
    }

    /// Эмулирует игровую кнопку «разложить по ближайшим сундукам».
    #[allow(dead_code)]
    pub fn quick_stack_to_nearby_chests(&self, player: &IDispatch) -> Result<()> {
        call(player, "QuickStackAllChests", &[])?;
        Ok(())
    }

    /// Жать «использовать предмет» будем из детура `Player.ItemCheck`,
    /// но само поле разрешаем уже сейчас.
    #[allow(dead_code)]
    pub fn set_control_use_item(&self, player: &IDispatch, pressed: bool) -> Result<()> {
        self.pl_control_use_item.set(player, Var::boolean(pressed))
    }

    /// Диагностика: версия игры из `Main.versionNumber`, если поле есть.
    pub fn version(&self) -> Option<String> {
        let field = Field::resolve(&self.main, "versionNumber").ok()?;
        field.get_static().ok()?.as_string()
    }
}
