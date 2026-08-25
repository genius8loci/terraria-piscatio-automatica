//! Типизированный доступ к состоянию Terraria 1.4.5.6.
//!
//! Имена полей и семантика проверены по декомпиляции конкретной версии,
//! см. `docs/research-1.4.5.6.md`.

use windows::core::Result;

use crate::clr::{
    BINDING_INSTANCE, BINDING_NON_PUBLIC, Clr, Field, Method, Type, Var, array_get, array_len, err,
};

/// Размер `Main.projectile` в 1.4.5.6.
const MAX_PROJECTILES: i32 = 1001;
/// Размер `Player.inventory` — 59; слоты 0..49 это основная сетка.
const INVENTORY_MAIN_SLOTS: i32 = 50;
/// `Player.maxBuffs` в 1.4.5.8.
const MAX_BUFFS: i32 = 44;

/// Зелья рыбалки: id предмета и id баффа (из ItemID / BuffID).
pub const POTIONS: [(i32, i32, &str); 3] = [
    (2354, 121, "Fishing"),
    (2355, 122, "Sonar"),
    (2356, 123, "Crate"),
];

/// Снимок состояния поплавка.
#[derive(Debug, Clone, Copy)]
pub struct Bobber {
    pub index: i32,
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
}

pub struct Game {
    _clr: Clr,

    f_my_player: Field,
    f_player: Field,
    f_projectile: Field,
    f_throttle: Field,
    f_version: Option<Field>,
    f_mouse_x: Field,
    f_mouse_y: Field,
    f_fish_drops: Field,

    /// Для получения адреса JIT-кода `Player.ItemCheck`.
    m_item_check: Method,
    m_get_method_handle: Method,
    m_get_function_pointer: Method,

    pr_active: Field,
    pr_bobber: Field,
    pr_owner: Field,
    pr_ai: Field,
    pr_local_ai: Field,

    pl_inventory: Field,
    pl_control_use_item: Field,
    pl_buff_type: Field,
    pl_quick_stack: Method,
    pl_add_buff: Method,

    m_object_get_type: Method,

    it_type: Field,
    it_stack: Field,
    it_bait: Field,
    it_buff_time: Field,
}

impl Game {
    pub fn attach(verbose: bool) -> Result<Game> {
        let clr = Clr::attach(verbose)?;

        let assembly = clr.assembly("Terraria", verbose)?;
        if verbose {
            crate::log!("шаг 4: сборка найдена — {}", assembly.full_name()?);
        }

        let main = assembly.get_type("Terraria.Main")?;
        let projectile = assembly.get_type("Terraria.Projectile")?;
        let player = assembly.get_type("Terraria.Player")?;
        let item = assembly.get_type("Terraria.Item")?;
        if verbose {
            crate::log!("шаг 5: типы Main/Projectile/Player/Item получены");
        }

        let mscorlib = clr.assembly("mscorlib", false)?;
        let method_base = mscorlib.get_type("System.Reflection.MethodBase")?;
        let method_handle = mscorlib.get_type("System.RuntimeMethodHandle")?;
        let object_type = mscorlib.get_type("System.Object")?;

        let game = Game {
            f_mouse_x: main.field("mouseX")?,
            f_mouse_y: main.field("mouseY")?,
            f_fish_drops: main.field("FishDropsDB")?,
            m_item_check: player.method("ItemCheck")?,
            m_get_method_handle: method_base.method("get_MethodHandle")?,
            m_get_function_pointer: method_handle.method("GetFunctionPointer")?,

            f_my_player: main.field("myPlayer")?,
            f_player: main.field("player")?,
            f_projectile: main.field("projectile")?,
            f_throttle: main.field("ThrottleWhenInactive")?,
            f_version: main.field("versionNumber").ok(),

            pr_active: projectile.field("active")?,
            pr_bobber: projectile.field("bobber")?,
            pr_owner: projectile.field("owner")?,
            pr_ai: projectile.field("ai")?,
            pr_local_ai: projectile.field("localAI")?,

            pl_inventory: player.field("inventory")?,
            pl_control_use_item: player.field("controlUseItem")?,
            pl_buff_type: player.field("buffType")?,
            pl_quick_stack: player.method("QuickStackAllChests")?,
            pl_add_buff: player.method("AddBuff")?,

            m_object_get_type: object_type.method("GetType")?,

            it_type: item.field("type")?,
            it_stack: item.field("stack")?,
            it_bait: item.field("bait")?,
            it_buff_time: item.field("buffTime")?,

            _clr: clr,
        };
        if verbose {
            crate::log!("шаг 6: все поля и методы разрешены");
        }
        Ok(game)
    }

    pub fn my_player(&self) -> Result<i32> {
        Ok(self.f_my_player.get_static()?.as_int().unwrap_or(-1))
    }

    /// `Main.player[Main.myPlayer]`.
    pub fn local_player(&self) -> Result<Option<Var>> {
        let index = self.my_player()?;
        if index < 0 {
            return Ok(None);
        }
        let players = self.f_player.get_static()?;
        let player = array_get(&players, index)?;
        if player.is_null() {
            return Ok(None);
        }
        Ok(Some(player))
    }

    /// Снимает троттлинг при потере фокуса: без этого свёрнутая игра
    /// спит по 20 мс на кадр и рыбалка идёт заметно медленнее.
    pub fn set_inactive_throttle(&self, enabled: bool) -> Result<()> {
        self.f_throttle.set_static(Var::boolean(enabled))
    }

    pub fn inactive_throttle(&self) -> Result<bool> {
        Ok(self.f_throttle.get_static()?.as_bool().unwrap_or(true))
    }

    /// Экранные координаты курсора игры.
    pub fn mouse(&self) -> Result<(i32, i32)> {
        Ok((
            self.f_mouse_x.get_static()?.as_int().unwrap_or(-1),
            self.f_mouse_y.get_static()?.as_int().unwrap_or(-1),
        ))
    }

    /// Адрес JIT-кода `Player.ItemCheck` — цель для детура.
    ///
    /// `MethodInfo.MethodHandle.GetFunctionPointer()` возвращает стабильную
    /// точку входа; метод вызывается каждый кадр, так что к моменту
    /// подключения он давно скомпилирован.
    pub fn item_check_address(&self) -> Result<usize> {
        let method = self.m_item_check.as_var();
        let handle = self.m_get_method_handle.invoke(&method, &[])?;
        let pointer = self.m_get_function_pointer.invoke(&handle, &[])?;
        pointer
            .as_ptr()
            .ok_or_else(|| crate::clr::err("GetFunctionPointer вернул не указатель"))
    }

    /// Тип объекта в рантайме — через `Object.GetType()`.
    fn type_of(&self, value: &Var) -> Result<Type> {
        self.m_object_get_type
            .invoke(value, &[])?
            .as_type()
            .ok_or_else(|| err("GetType вернул не тип"))
    }

    /// Полный список предметов, которые вообще можно выловить.
    ///
    /// Берётся из самой игры: `Main.FishDropsDB` — это `FishDropRuleList`
    /// с приватным `List<FishDropRule>`, а у каждого правила есть
    /// `public int[] PossibleItems`. Никаких зашитых списков.
    pub fn fishable_items(&self) -> Result<Vec<i32>> {
        let db = self.f_fish_drops.get_static()?;
        if db.is_null() {
            return Err(err("Main.FishDropsDB ещё не заполнен"));
        }
        let rules_field = self
            .type_of(&db)?
            .field_flags("_rules", BINDING_NON_PUBLIC | BINDING_INSTANCE)?;
        let list = rules_field.get(&db)?;
        let array = self.type_of(&list)?.method("ToArray")?.invoke(&list, &[])?;

        let count = array_len(&array)?;
        let mut items: Vec<i32> = Vec::new();
        let mut possible: Option<Field> = None;

        for i in 0..count {
            let rule = array_get(&array, i)?;
            if rule.is_null() {
                continue;
            }
            if possible.is_none() {
                possible = Some(self.type_of(&rule)?.field("PossibleItems")?);
            }
            let Some(field) = possible.as_ref() else {
                continue;
            };
            let ids = field.get(&rule)?;
            if ids.is_null() {
                continue;
            }
            let n = array_len(&ids).unwrap_or(0);
            for j in 0..n {
                if let Some(id) = array_get(&ids, j)?.as_int() {
                    if id > 0 && !items.contains(&id) {
                        items.push(id);
                    }
                }
            }
        }
        items.sort_unstable();
        Ok(items)
    }

    pub fn has_buff(&self, player: &Var, buff: i32) -> Result<bool> {
        let buffs = self.pl_buff_type.get(player)?;
        for i in 0..MAX_BUFFS {
            if array_get(&buffs, i)?.as_int().unwrap_or(0) == buff {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Слот инвентаря с предметом нужного типа и ненулевым стаком.
    pub fn find_item(&self, player: &Var, item_type: i32) -> Result<Option<i32>> {
        let inventory = self.pl_inventory.get(player)?;
        for i in 0..INVENTORY_MAIN_SLOTS {
            let item = array_get(&inventory, i)?;
            if item.is_null() {
                continue;
            }
            if self.it_type.get(&item)?.as_int().unwrap_or(0) != item_type {
                continue;
            }
            if self.it_stack.get(&item)?.as_int().unwrap_or(0) > 0 {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    /// Выпить зелье из слота: вешаем бафф на его штатную длительность
    /// и тратим одну штуку.
    pub fn drink(&self, player: &Var, slot: i32, buff: i32) -> Result<()> {
        let inventory = self.pl_inventory.get(player)?;
        let item = array_get(&inventory, slot)?;
        if item.is_null() {
            return Err(err("слот пуст"));
        }
        let duration = self.it_buff_time.get(&item)?.as_int().unwrap_or(0);
        if duration <= 0 {
            return Err(err("у предмета нет длительности баффа"));
        }
        self.pl_add_buff.invoke(
            player,
            &[Var::int(buff), Var::int(duration), Var::boolean(false)],
        )?;
        let stack = self.it_stack.get(&item)?.as_int().unwrap_or(0);
        self.it_stack.set(&item, Var::int(stack - 1))?;
        Ok(())
    }

    pub fn version(&self) -> Option<String> {
        self.f_version.as_ref()?.get_static().ok()?.as_string()
    }

    /// Ищет поплавок локального игрока. У игрока он всегда один —
    /// наличие поплавка запрещает новый заброс (см. research-док).
    ///
    /// `hint` — индекс с прошлого раза. Полный перебор 1001 снаряда стоит
    /// пары тысяч COM-вызовов, поэтому сначала проверяем known-индекс.
    pub fn find_bobber(&self, hint: Option<i32>) -> Result<Option<Bobber>> {
        let me = self.my_player()?;
        if me < 0 {
            return Ok(None);
        }
        let projectiles = self.f_projectile.get_static()?;

        if let Some(i) = hint {
            if (0..MAX_PROJECTILES).contains(&i) {
                if let Some(bobber) = self.read_bobber(&projectiles, i, me)? {
                    return Ok(Some(bobber));
                }
            }
        }

        for i in 0..MAX_PROJECTILES {
            if Some(i) == hint {
                continue;
            }
            if let Some(bobber) = self.read_bobber(&projectiles, i, me)? {
                return Ok(Some(bobber));
            }
        }
        Ok(None)
    }

    fn read_bobber(&self, projectiles: &Var, i: i32, me: i32) -> Result<Option<Bobber>> {
        {
            let projectile = array_get(projectiles, i)?;
            if projectile.is_null() {
                return Ok(None);
            }
            if !self.pr_active.get(&projectile)?.as_bool().unwrap_or(false) {
                return Ok(None);
            }
            if !self.pr_bobber.get(&projectile)?.as_bool().unwrap_or(false) {
                return Ok(None);
            }
            if self.pr_owner.get(&projectile)?.as_int().unwrap_or(-1) != me {
                return Ok(None);
            }

            let ai = self.pr_ai.get(&projectile)?;
            let local_ai = self.pr_local_ai.get(&projectile)?;

            Ok(Some(Bobber {
                index: i,
                ai1: array_get(&ai, 1)?.as_float().unwrap_or(0.0),
                local_ai1: array_get(&local_ai, 1)?.as_float().unwrap_or(0.0),
            }))
        }
    }

    /// Суммарный стак наживки в инвентаре. Логика повторяет
    /// `Player.Fishing_GetBait`: предмет считается наживкой при `bait > 0`.
    pub fn bait_total(&self, player: &Var) -> Result<i32> {
        let inventory = self.pl_inventory.get(player)?;
        let mut total = 0;
        for i in 0..INVENTORY_MAIN_SLOTS {
            let item = array_get(&inventory, i)?;
            if item.is_null() {
                continue;
            }
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
    pub fn free_slots(&self, player: &Var) -> Result<i32> {
        let inventory = self.pl_inventory.get(player)?;
        let mut free = 0;
        for i in 0..INVENTORY_MAIN_SLOTS {
            let item = array_get(&inventory, i)?;
            if item.is_null() {
                free += 1;
                continue;
            }
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
    pub fn quick_stack_to_nearby_chests(&self, player: &Var) -> Result<()> {
        self.pl_quick_stack.invoke(player, &[])?;
        Ok(())
    }

    /// Жать «использовать предмет» будем из детура `Player.ItemCheck`,
    /// но поле разрешаем уже сейчас.
    #[allow(dead_code)]
    pub fn set_control_use_item(&self, player: &Var, pressed: bool) -> Result<()> {
        self.pl_control_use_item.set(player, Var::boolean(pressed))
    }
}
