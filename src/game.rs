//! Типизированный доступ к состоянию Terraria 1.4.5.8.
//!
//! Имена полей и семантика проверены по декомпиляции конкретной версии,
//! см. `docs/index.md`.

use windows::core::Result;

use crate::clr::{
    BINDING_INSTANCE, BINDING_NON_PUBLIC, Clr, Field, Method, Type, Var, array_get, array_len, err,
};

/// Запасной размер `Main.projectile`: в 1.4.5.8 массив на 1001 снаряд.
/// Настоящую длину спрашиваем у самого массива, это лишь откат на случай,
/// если она почему-то не прочиталась.
const MAX_PROJECTILES: i32 = 1001;
/// Размер `Player.inventory` — 59; слоты 0..49 это основная сетка.
const INVENTORY_MAIN_SLOTS: i32 = 50;
/// Запасное значение `Player.maxBuffs` (44 в 1.4.5.8): настоящую длину
/// берём у самого массива баффов.
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
    /// `Projectile.wet` — поплавок уже в воде. Пока нет, он ещё летит.
    pub wet: bool,
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
    /// `Main.ThrottleWhenInactive`. Необязательно: без него просто не станем
    /// трогать сон свёрнутой игры — своя выдержка тиков есть в `input`.
    f_throttle: Option<Field>,
    /// `Main.drawingPlayerChat` — открыт ли чат игры. По нему хоткеи молчат:
    /// иначе стрелки листали бы историю чата и заодно дёргали панель,
    /// а Delete при правке строки выгружал бы DLL.
    f_chat_open: Option<Field>,
    /// `Main.netMode` — 0 в одиночной игре. По нему решается, можно ли
    /// ставить нажатие прямой записью в объект игрока: в сетевой `ItemCheck`
    /// вызывается за каждого, и `this` там не обязательно наш.
    f_net_mode: Option<Field>,
    f_version: Option<Field>,
    f_mouse_x: Field,
    f_mouse_y: Field,
    /// `Main.FishDropsDB` — список всего ловимого. Необязательно: без него
    /// не будет сетки фильтра и иконок, но ловля в режиме чёрного списка
    /// (берём всё) работает.
    f_fish_drops: Option<Field>,
    /// `Main.itemAnimations` — по элементу на предмет, `null` у неподвижных.
    /// Необязательно: без него у анимированных иконок в ячейку попадёт вся
    /// лента кадров вместо одного, и только.
    f_item_animations: Option<Field>,
    /// `ItemID.Sets.IsFishingCrate` — таблица «это ящик», по id предмета.
    f_crate_set: Option<Field>,
    /// Свой экземпляр `Item` под расспросы об именах, и как его настроить.
    scratch_item: Option<Var>,
    m_net_defaults: Option<Method>,
    m_affix_name: Option<Method>,
    m_item_clone: Option<Method>,

    /// Для получения адреса JIT-кода `Player.ItemCheck`.
    m_item_check: Method,
    /// `Main.DrawCursor` — цель второго детура. Необязателен: он и так
    /// выключается флагом в конфиге, а без него панель просто уходит
    /// в `Present` и рисует курсор сама.
    m_draw_cursor: Option<Method>,
    m_get_method_handle: Method,
    m_get_function_pointer: Method,

    pr_active: Field,
    pr_bobber: Field,
    pr_owner: Field,
    pr_ai: Field,
    pr_local_ai: Field,
    /// `Projectile.wet` — поплавок коснулся воды. Необязательно: не нашлось —
    /// панель просто не станет различать полёт и воду.
    pr_wet: Option<Field>,

    pl_inventory: Field,
    pl_buff_type: Field,
    // `QuickStackAllChests` и `controlUseItem` здесь намеренно нет: и то
    // и другое трогается только с игрового потока, их хэндлы живут в `input`.
    /// `Player.AddBuff` — им вешается баф выпитого зелья. Вместе
    /// с `it_buff_time` необязателен: не нашлись — не будет автопитья,
    /// оно и по умолчанию выключено.
    pl_add_buff: Option<Method>,
    /// `Player.HeldItem` — предмет в выбранной ячейке хотбара. Это свойство
    /// (`inventory[selectedItem]`), а не поле, поэтому зовём геттер.
    /// Необязателен: не нашёлся — просто не будет проверки удочки в руке,
    /// всё остальное работает.
    m_held_item: Option<Method>,
    /// `Language.ActiveCulture` и `GameCulture.LegacyId` — по ним выбирается
    /// язык панели. Тоже необязательны: не нашлись — останется язык
    /// по умолчанию.
    m_active_culture: Option<Method>,
    f_legacy_id: Option<Field>,
    /// `Item.questItem` и `Lang.GetNPCNameValue` — под подписи в чате.
    it_quest_item: Option<Field>,
    m_npc_name: Option<Method>,

    m_object_get_type: Method,

    it_type: Field,
    it_stack: Field,
    it_bait: Field,
    /// `Item.buffTime` — штатная длительность бафа зелья. См. `pl_add_buff`.
    it_buff_time: Option<Field>,
    /// `Item.fishingPole` — сила удочки; у всего остального ноль. Ровно по
    /// нему игра и отличает удочку от прочего инвентаря.
    it_fishing_pole: Field,
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
            f_fish_drops: main.field("FishDropsDB").ok(),
            f_item_animations: main.field("itemAnimations").ok(),
            // Вложенный тип в рефлексии пишется через плюс. Без него
            // пропадёт только счётчик ящиков, поэтому ошибку глотаем.
            f_crate_set: assembly
                .get_type("Terraria.ID.ItemID+Sets")
                .ok()
                .and_then(|sets| sets.field("IsFishingCrate").ok()),
            scratch_item: None,
            m_net_defaults: item.method("netDefaults").ok(),
            m_affix_name: item.method("AffixName").ok(),
            m_item_clone: item.method("Clone").ok(),
            m_item_check: player.method("ItemCheck")?,
            m_draw_cursor: main.method("DrawCursor").ok(),
            m_get_method_handle: method_base.method("get_MethodHandle")?,
            m_get_function_pointer: method_handle.method("GetFunctionPointer")?,

            f_my_player: main.field("myPlayer")?,
            f_player: main.field("player")?,
            f_projectile: main.field("projectile")?,
            f_throttle: main.field("ThrottleWhenInactive").ok(),
            f_chat_open: main.field("drawingPlayerChat").ok(),
            f_net_mode: main.field("netMode").ok(),
            f_version: main.field("versionNumber").ok(),

            pr_active: projectile.field("active")?,
            pr_bobber: projectile.field("bobber")?,
            pr_owner: projectile.field("owner")?,
            pr_ai: projectile.field("ai")?,
            pr_local_ai: projectile.field("localAI")?,
            pr_wet: projectile.field("wet").ok(),

            pl_inventory: player.field("inventory")?,
            pl_buff_type: player.field("buffType")?,
            pl_add_buff: player.method("AddBuff").ok(),
            m_held_item: player.method("get_HeldItem").ok(),
            m_active_culture: assembly
                .get_type("Terraria.Localization.Language")
                .ok()
                .and_then(|t| t.method("get_ActiveCulture").ok()),
            f_legacy_id: assembly
                .get_type("Terraria.Localization.GameCulture")
                .ok()
                .and_then(|t| t.field("LegacyId").ok()),
            it_quest_item: item.field("questItem").ok(),
            m_npc_name: assembly
                .get_type("Terraria.Lang")
                .ok()
                .and_then(|t| t.method("GetNPCNameValue").ok()),

            m_object_get_type: object_type.method("GetType")?,

            it_type: item.field("type")?,
            it_stack: item.field("stack")?,
            it_bait: item.field("bait")?,
            it_buff_time: item.field("buffTime").ok(),
            it_fishing_pole: item.field("fishingPole")?,

            _clr: clr,
        };
        if game.m_held_item.is_none() {
            crate::log!(
                "внимание: геттер Player.HeldItem не найден — смену предмета \
                 в хотбаре автомат не заметит"
            );
        }
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

    /// Снимает сон игры при потере фокуса.
    ///
    /// Сам по себе он безобиден, но у свёрнутой игры это **единственный**
    /// ограничитель: кадры не рисуются, значит и вертикальной синхронизации
    /// в `Present` нет, а при `FrameSkipMode != 0` шаг цикла не фиксирован.
    /// Снятый сон разгоняет мир в десятки раз, поэтому вместе с этим вызовом
    /// обязана работать своя выдержка тиков — см. `input::pace_inactive`.
    pub fn set_inactive_throttle(&self, enabled: bool) -> Result<()> {
        let field = self
            .f_throttle
            .as_ref()
            .ok_or_else(|| err("поля Main.ThrottleWhenInactive нет"))?;
        field.set_static(Var::boolean(enabled))
    }

    pub fn inactive_throttle(&self) -> Result<bool> {
        let field = self
            .f_throttle
            .as_ref()
            .ok_or_else(|| err("поля Main.ThrottleWhenInactive нет"))?;
        Ok(field.get_static()?.as_bool().unwrap_or(true))
    }

    /// Одиночная ли игра. Поля нет — отвечаем «нет»: в сомнительном случае
    /// лучше остаться на рефлексии, чем писать байт в чужого игрока.
    pub fn single_player(&self) -> bool {
        self.f_net_mode
            .as_ref()
            .and_then(|f| f.get_static().ok())
            .and_then(|v| v.as_int())
            == Some(0)
    }

    /// Открыт ли чат игры. Пока открыт, клавиши принадлежат ему, а не нам.
    /// Поля нет — считаем, что закрыт: это прежнее поведение.
    pub fn chat_open(&self) -> bool {
        self.f_chat_open
            .as_ref()
            .and_then(|f| f.get_static().ok())
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
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
        self.jit_address(&self.m_item_check)
    }

    /// `Main.DrawCursor` — точка, где интерфейс уже выгружен, а курсор ещё нет.
    pub fn draw_cursor_address(&self) -> Result<usize> {
        let method = self
            .m_draw_cursor
            .as_ref()
            .ok_or_else(|| err("метода Main.DrawCursor нет"))?;
        self.jit_address(method)
    }

    /// Адрес машинного кода метода: `MethodBase.MethodHandle.GetFunctionPointer()`.
    fn jit_address(&self, method: &Method) -> Result<usize> {
        let handle = self.m_get_method_handle.invoke(&method.as_var(), &[])?;
        let pointer = self.m_get_function_pointer.invoke(&handle, &[])?;
        pointer
            .as_ptr()
            .ok_or_else(|| crate::clr::err("GetFunctionPointer вернул не указатель"))
    }

    /// Сколько кадров в картинке предмета.
    ///
    /// У анимированных предметов `Content/Images/Item_<id>.xnb` — не один
    /// спрайт, а лента кадров сверху вниз; сколько их, знает только игра,
    /// в `Main.itemAnimations[id].FrameCount`. У неподвижных там `null`,
    /// и кадр ровно один.
    pub fn item_frames(&self, item: i32) -> Result<u32> {
        let Some(field) = self.f_item_animations.as_ref() else {
            return Ok(1);
        };
        let animations = field.get_static()?;
        if animations.is_null() || item < 0 || item >= array_len(&animations)? {
            return Ok(1);
        }
        let animation = array_get(&animations, item)?;
        if animation.is_null() {
            return Ok(1);
        }
        let frames = self
            .type_of(&animation)?
            .field("FrameCount")?
            .get(&animation)?
            .as_int()
            .unwrap_or(1);
        Ok(frames.max(1) as u32)
    }

    /// Ящик ли это. Список держит сама игра — `ItemID.Sets.IsFishingCrate`.
    pub fn is_crate(&self, item: i32) -> bool {
        let Some(field) = self.f_crate_set.as_ref() else {
            return false;
        };
        let Ok(set) = field.get_static() else {
            return false;
        };
        if set.is_null() || item < 0 || item >= array_len(&set).unwrap_or(0) {
            return false;
        }
        array_get(&set, item)
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Имя предмета так, как его показывает игра. Нужно поиску по фильтру.
    ///
    /// Экземпляр `Item` заводим один раз клоном из массива предметов игрока
    /// и дальше перенастраиваем: `netDefaults` — единственная неперегруженная
    /// настройка по id, `SetDefaults` на `GetMethod(String)` бросает
    /// `AmbiguousMatchException`.
    /// Заодно и признак квестовой рыбы: `Item.questItem`. Читается с того же
    /// экземпляра, что и имя, поэтому лишней настройки предмета не выходит.
    pub fn item_facts(&mut self, id: i32) -> Option<(String, bool)> {
        let (net_defaults, affix) = (self.m_net_defaults.as_ref()?, self.m_affix_name.as_ref()?);
        if self.scratch_item.is_none() {
            self.scratch_item = Some(self.spare_item()?);
        }
        let item = self.scratch_item.as_ref()?;
        net_defaults.invoke(item, &[Var::int(id)]).ok()?;
        let name = affix.invoke(item, &[]).ok()?.as_string()?;
        let quest = self
            .it_quest_item
            .as_ref()
            .and_then(|f| f.get(item).ok())
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Some((name, quest))
    }

    /// Имя противника по его id — им подписываются спавны в чате.
    pub fn npc_name(&self, net_id: i32) -> Option<String> {
        self.m_npc_name
            .as_ref()?
            .invoke(&Var::null(), &[Var::int(net_id)])
            .ok()?
            .as_string()
    }

    /// Свободный экземпляр `Item`: берём копию первой ячейки инвентаря.
    fn spare_item(&self) -> Option<Var> {
        let clone = self.m_item_clone.as_ref()?;
        let player = self.local_player().ok()??;
        let inventory = self.pl_inventory.get(&player).ok()?;
        let first = array_get(&inventory, 0).ok()?;
        if first.is_null() {
            return None;
        }
        clone.invoke(&first, &[]).ok()
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
        let field = self
            .f_fish_drops
            .as_ref()
            .ok_or_else(|| err("поля Main.FishDropsDB нет"))?;
        let db = field.get_static()?;
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
        // Поле кэшируем, но не считаем его годным навсегда: правила бывают
        // разных типов, и у соседнего `PossibleItems` может лежать в другом
        // месте. Сорвалось чтение — перерешаем поле по типу этого правила,
        // а не роняем весь список.
        let mut possible: Option<Field> = None;

        for i in 0..count {
            let rule = array_get(&array, i)?;
            if rule.is_null() {
                continue;
            }
            let mut ids = possible.as_ref().and_then(|f| f.get(&rule).ok());
            if ids.is_none() {
                let Ok(field) = self.type_of(&rule).and_then(|t| t.field("PossibleItems")) else {
                    continue;
                };
                ids = field.get(&rule).ok();
                possible = Some(field);
            }
            let Some(ids) = ids else {
                continue;
            };
            if ids.is_null() {
                continue;
            }
            let n = array_len(&ids).unwrap_or(0);
            for j in 0..n {
                if let Ok(slot) = array_get(&ids, j)
                    && let Some(id) = slot.as_int()
                    && id > 0
                    && !items.contains(&id)
                {
                    items.push(id);
                }
            }
        }
        items.sort_unstable();
        Ok(items)
    }

    pub fn has_buff(&self, player: &Var, buff: i32) -> Result<bool> {
        let buffs = self.pl_buff_type.get(player)?;
        let count = array_len(&buffs).unwrap_or(MAX_BUFFS);
        for i in 0..count {
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
        let (Some(add_buff), Some(buff_time)) =
            (self.pl_add_buff.as_ref(), self.it_buff_time.as_ref())
        else {
            return Err(err("Player.AddBuff или Item.buffTime не нашлись"));
        };
        let inventory = self.pl_inventory.get(player)?;
        let item = array_get(&inventory, slot)?;
        if item.is_null() {
            return Err(err("слот пуст"));
        }
        let duration = buff_time.get(&item)?.as_int().unwrap_or(0);
        if duration <= 0 {
            return Err(err("у предмета нет длительности баффа"));
        }
        add_buff.invoke(
            player,
            &[Var::int(buff), Var::int(duration), Var::boolean(false)],
        )?;
        let stack = self.it_stack.get(&item)?.as_int().unwrap_or(0);
        let left = stack - 1;
        self.it_stack.set(&item, Var::int(left))?;
        // Пустая ячейка у игры — это `type == 0`, а не «стак ноль»: последнее
        // она сама доводит через `Item.TurnToAir()`. Без обнуления типа
        // в инвентаре оставался бы призрак зелья, который рисуется, но
        // не берётся.
        if left <= 0 {
            self.it_type.set(&item, Var::int(0))?;
        }
        Ok(())
    }

    /// Держит ли игрок удочку. Пустая ячейка хотбара и любой другой предмет
    /// дают `false`: `Item.fishingPole` там ноль.
    ///
    /// Если геттера `HeldItem` не нашлось, отвечаем «удочка»: не знать —
    /// не повод выключать рыбалку.
    pub fn holding_rod(&self, player: &Var) -> Result<bool> {
        let Some(held_item) = self.m_held_item.as_ref() else {
            return Ok(true);
        };
        let held = held_item.invoke(player, &[])?;
        if held.is_null() {
            return Ok(false);
        }
        let power = self.it_fishing_pole.get(&held)?.as_int().unwrap_or(0);
        Ok(power > 0)
    }

    /// Язык игры — `GameCulture.LegacyId`. У русского он шестой,
    /// см. `GameCulture.CultureName`.
    pub fn culture_id(&self) -> Option<i32> {
        let culture = self
            .m_active_culture
            .as_ref()?
            .invoke(&Var::null(), &[])
            .ok()?;
        if culture.is_null() {
            return None;
        }
        self.f_legacy_id.as_ref()?.get(&culture).ok()?.as_int()
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
        let count = array_len(&projectiles).unwrap_or(MAX_PROJECTILES);

        if let Some(i) = hint
            && (0..count).contains(&i)
            && let Some(bobber) = self.read_bobber(&projectiles, i, me)?
        {
            return Ok(Some(bobber));
        }

        for i in 0..count {
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
            wet: self
                .pr_wet
                .as_ref()
                .and_then(|f| f.get(&projectile).ok())
                .and_then(|v| v.as_bool())
                // Поля нет — считаем, что уже долетел: так панель ведёт
                // себя как раньше, а не врёт про вечный полёт.
                .unwrap_or(true),
        }))
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
}
