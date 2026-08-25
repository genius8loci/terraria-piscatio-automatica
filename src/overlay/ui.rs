//! Раскладка интерфейса, попадания мыши и построение списка отрисовки.
//!
//! Режим непосредственный: каждый кадр заново считаем геометрию и тут же
//! проверяем попадания. Состояние — только «раскрыто», «какая вкладка»
//! и прокрутка фильтра, всё остальное живёт в `state::Shared`.
//!
//! Рисуем родными текстурами игры: окна и кнопки собираются девятичастной
//! нарезкой, ячейки и переключатели — целыми спрайтами. Поэтому цвета здесь
//! почти всегда белые: свой цвет у графики уже внутри.

use super::state::{self, Mark};
use super::{Painter, colors, icons};

// Заголовок панели собирается из `Cargo.toml`, чтобы не разъезжаться
// со свойствами самой DLL: там те же `description`, `version` и `authors`.
// Три куска — три цвета, иначе строка сливается в одну кашу.
const NAME: &str = concat!(env!("CARGO_PKG_DESCRIPTION"), " ");
const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"), " ");
const AUTHOR: &str = concat!("by ", env!("CARGO_PKG_AUTHORS"));
/// Заголовок целиком — по нему считается ширина панели.
const TITLE: &str = concat!(
    env!("CARGO_PKG_DESCRIPTION"),
    " v",
    env!("CARGO_PKG_VERSION"),
    " by ",
    env!("CARGO_PKG_AUTHORS")
);

/// Насколько панель плотнее родного интерфейса игры.
const DENSITY: f32 = 0.9;

/// Подписи строк — по самой длинной из них считается ширина панели.
/// У авторыбалки берётся с самым длинным припиской, иначе координаты
/// заброса налезали бы на звёздочку.
const LABELS: &[&str] = &[
    "Авторыбалка (зафиксировано 1920:1080)",
    "Сундуки разложить при заполнении",
    "Подсекать врагов (Герцог Рыброн)",
    "Поплавок",
    "Свободные ячейки",
    "Автопитьё зелий",
    "Зелья для автоиспользования:",
];

/// Базовые размеры при масштабе 1.0; всё остальное — умножением.
const ROW_H: f32 = 34.0;
/// Нижний предел ширины: панель уже этого выглядит обрубком, даже если
/// весь текст в неё влез.
const PANEL_MIN_W: f32 = 380.0;
const PAD: f32 = 12.0;
const GAP: f32 = 6.0;
const ARROW_W: f32 = 64.0;
const ARROW_H: f32 = 26.0;
/// Переключатель рисуется в натуральную величину текстуры: 14 пикселей.
const TOGGLE: f32 = 14.0;
/// Доля ячейки под крестик «пропускаю».
const CROSS_SIZE: f32 = 0.7;
/// Сторона уголка на кнопке сворачивания.
const CHEVRON: f32 = 16.0;
/// Подсказка в пустой строке поиска — как у игры в её собственных полях.
const SEARCH_HINT: &str = "Имя:";
/// Полпериода мигания курсора ввода, в кадрах.
const BLINK: u32 = 30;
/// Поле от нижнего края экрана, ниже которого окно фильтра не растёт.
const SCREEN_MARGIN: f32 = 24.0;
const BAR_W: f32 = 20.0;
/// Высота, нужная при масштабе 1.0, чтобы уместились основное окно,
/// шапка фильтра и хотя бы один ряд ячеек. Ниже этого масштаб приходится
/// сбавлять, иначе фильтр уезжает за нижний край экрана.
const NEEDED_HEIGHT: f32 = 580.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    None,
    Filter,
    Stats,
}

pub struct UiState {
    pub expanded: bool,
    pub tab: Tab,
    /// Первая видимая строка сетки фильтра.
    pub filter_row: usize,
    /// Ползунок прокрутки схвачен: сколько было от его верха до курсора.
    pub drag: Option<f32>,
    /// Что набрано в строке поиска, и стоит ли в ней курсор.
    pub search: String,
    pub search_focus: bool,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            expanded: true,
            tab: Tab::None,
            filter_row: 0,
            drag: None,
            search: String::new(),
            search_focus: false,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Input {
    pub x: f32,
    pub y: f32,
    /// Кнопка нажата именно в этом кадре.
    pub clicked: bool,
    /// Кнопка держится — по этому тянут ползунок.
    pub down: bool,
    /// Щелчки колеса за кадр: вверх положительные.
    pub wheel: i32,
}

/// Что кадр рассказал наружу.
#[derive(Clone, Copy, Default)]
pub struct Frame {
    /// Курсор над окном — клик не должен уходить в игру.
    pub over_ui: bool,
    /// Предмет под курсором; `0` — ничего.
    pub hover_item: i32,
    /// Курсор стоит в строке поиска: клавиши сейчас про текст.
    pub typing: bool,
}

#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

pub struct Layout<'a, 'b> {
    painter: &'a mut Painter<'b>,
    input: Input,
    scale: f32,
    /// Номер кадра — по нему моргает курсор ввода.
    frames: u32,
    /// Курсор попал хоть в одну нашу область — игре клик отдавать нельзя.
    pub over_ui: bool,
    /// Предмет под курсором — по нему покажем подсказку игры.
    pub hover_item: i32,
}

impl<'a, 'b> Layout<'a, 'b> {
    fn hit(&mut self, r: Rect) -> bool {
        if !r.contains(self.input.x, self.input.y) {
            return false;
        }
        self.over_ui = true;
        self.input.clicked
    }

    fn hovered(&self, r: Rect) -> bool {
        r.contains(self.input.x, self.input.y)
    }

    /// Золотая рамка поверх области — так игра показывает наведение
    /// и фокус в бестиарии.
    fn frame(&mut self, r: Rect, id: i32) {
        self.painter
            .nine_slice(id, r.x, r.y, r.w, r.h, icons::FRAME_INSET, colors::PLAIN);
    }

    /// Мигание курсора ввода: примерно раз в полсекунды, как у игры.
    fn blink(&self) -> bool {
        self.frames % (BLINK * 2) < BLINK
    }

    /// Ячейка предмета: попадание плюс заявка на подсказку.
    fn hit_item(&mut self, r: Rect, item: i32) -> bool {
        if self.hovered(r) {
            self.hover_item = item;
        }
        self.hit(r)
    }

    /// Окно: фон и обводка поверх него — ровно так рисует панели игра.
    /// Заодно занимает мышь целиком: щели между строками — тоже наша
    /// территория, клик сквозь них уходить в мир не должен.
    fn panel(&mut self, r: Rect) {
        if self.hovered(r) {
            self.over_ui = true;
        }
        self.painter.nine_slice(
            icons::PANEL,
            r.x,
            r.y,
            r.w,
            r.h,
            icons::PANEL_INSET,
            colors::PANEL,
        );
        self.painter.nine_slice(
            icons::PANEL_BORDER,
            r.x,
            r.y,
            r.w,
            r.h,
            icons::PANEL_INSET,
            colors::PANEL_BORDER,
        );
    }

    /// Строка внутри окна — та же подложка, что у списков игры.
    fn row_bg(&mut self, r: Rect) {
        self.painter.nine_slice(
            icons::INNER_PANEL,
            r.x,
            r.y,
            r.w,
            r.h,
            icons::INNER_INSET,
            colors::PLAIN,
        );
    }

    /// Ячейка предмета: подложка, иконка и отметка поверх.
    ///
    /// Отметки сделаны так же, как их показывает игра. «Беру» — светлая
    /// рамка `Inventory_Back15`, зелёная: ровно ей игра подсвечивает новые
    /// предметы в инвентаре и слоты, куда предмет положить можно.
    /// «Пропускаю» — та же рамка красным, предмет притушен и перечёркнут,
    /// как недоступное в меню дублирования.
    fn item_cell(&mut self, r: Rect, item: i32, mark: Mark) {
        let hovered = self.hovered(r);
        let (backing, tint) = match mark {
            Mark::Allow => (icons::SLOT_MARK, colors::RARE_GREEN),
            Mark::Deny => (icons::SLOT_MARK, colors::RARE_RED),
            Mark::Neutral if hovered => (icons::SLOT_HOVER, colors::SLOT),
            Mark::Neutral => (icons::SLOT, colors::SLOT),
        };
        self.painter.stretch(backing, r.x, r.y, r.w, r.h, tint);
        let icon = if mark == Mark::Deny {
            colors::ICON_DENIED
        } else {
            colors::PLAIN
        };
        self.painter.icon(item, r.x, r.y, r.w, r.h, icon);
        if mark == Mark::Deny {
            let size = (r.w * CROSS_SIZE).round();
            self.painter.stretch(
                icons::CROSS,
                (r.x + (r.w - size) * 0.5).round(),
                (r.y + (r.h - size) * 0.5).round(),
                size,
                size,
                colors::CROSS,
            );
        }
    }

    /// Переключатель — звёздочка ранга из бестиария: золотая включено,
    /// тусклая выключено. Цвет у обеих свой, поэтому красим их белым;
    /// наведение показываем золотой рамкой, как игра.
    fn toggle(&mut self, r: Rect, on: bool) -> bool {
        let clicked = self.hit(r);
        let id = if on {
            icons::TOGGLE_ON
        } else {
            icons::TOGGLE_OFF
        };
        self.painter.stretch(id, r.x, r.y, r.w, r.h, colors::PLAIN);
        if self.hovered(r) {
            let pad = (2.0 * self.scale).round();
            self.frame(
                Rect {
                    x: r.x - pad,
                    y: r.y - pad,
                    w: r.w + pad * 2.0,
                    h: r.h + pad * 2.0,
                },
                icons::FRAME_SMALL,
            );
        }
        clicked
    }

    /// Переключатель, прижатый к правому краю строки, вместе с подписью слева.
    fn switch_row(&mut self, r: Rect, label: &str, on: bool) -> bool {
        self.switch_row_note(r, label, "", colors::TEXT, on)
    }

    /// То же, но с приписком своего цвета сразу за подписью.
    fn switch_row_note(
        &mut self,
        r: Rect,
        label: &str,
        note: &str,
        note_color: u32,
        on: bool,
    ) -> bool {
        self.row_bg(r);
        let pad = (PAD * self.scale).round();
        self.painter
            .text_left(r.x + pad, r.y, r.h, label, colors::TEXT);
        if !note.is_empty() {
            let after = r.x + pad + self.painter.measure(label);
            self.painter.text_left(after, r.y, r.h, note, note_color);
        }
        let size = (TOGGLE * self.scale).round();
        let knob = Rect {
            x: r.x + r.w - size - pad,
            y: (r.y + (r.h - size) * 0.5).round(),
            w: size,
            h: size,
        };
        self.toggle(knob, on)
    }

    /// Строка «подпись — значение».
    fn value_row(&mut self, r: Rect, label: &str, value: &str, color: u32) {
        self.row_bg(r);
        let pad = (PAD * self.scale).round();
        self.painter
            .text_left(r.x + pad, r.y, r.h, label, colors::TEXT);
        self.painter
            .text_right(r.x, r.y, r.w - pad, r.h, value, color);
    }

    /// Запасной курсор: нужен, только если панель рисуется из `Present`,
    /// то есть после того, как игра нарисовала свой — он остался под панелью.
    fn draw_cursor(&mut self) {
        if self.input.x < 0.0 || self.input.y < 0.0 {
            return;
        }
        self.painter
            .sprite(icons::CURSOR, self.input.x, self.input.y, colors::PLAIN);
    }

    /// Кнопка собирается из той же панели, что и окна: `ButtonBacking`
    /// скруглён заметно мельче, и рядом с окном кнопка на нём смотрелась
    /// чужой. Разница только в заливке — по ней и видно состояние.
    fn button(&mut self, r: Rect, label: &str, active: bool) -> bool {
        let clicked = self.hit(r);
        let fill = if active {
            colors::BUTTON_ACTIVE
        } else if self.hovered(r) {
            colors::BUTTON_HOVER
        } else {
            colors::BUTTON
        };
        self.painter
            .nine_slice(icons::PANEL, r.x, r.y, r.w, r.h, icons::PANEL_INSET, fill);
        self.painter.nine_slice(
            icons::PANEL_BORDER,
            r.x,
            r.y,
            r.w,
            r.h,
            icons::PANEL_INSET,
            colors::PANEL_BORDER,
        );
        self.painter
            .text_centered(r.x, r.y, r.w, r.h, label, colors::TEXT);
        clicked
    }
}

/// Строит кадр интерфейса.
pub fn build(
    painter: &mut Painter,
    ui: &mut UiState,
    input: Input,
    screen: (f32, f32),
    ui_scale: f32,
    own_cursor: bool,
    frames: u32,
) -> Frame {
    // Растём вместе с интерфейсом игры: масштаб — её собственная настройка.
    // Чуть плотнее родного: у нас строки текста, а не ряды ячеек, и в один
    // к одному панель выглядит рядом с игровым интерфейсом громоздкой.
    // Сверху ограничены высотой экрана, иначе окно фильтра уедет за край;
    // на обычных разрешениях этот предел не срабатывает.
    let scale = (ui_scale * DENSITY)
        .clamp(0.5, 3.0)
        .min(screen.1 / NEEDED_HEIGHT)
        .max(0.5);
    painter.scale = scale;
    let mut layout = Layout {
        painter,
        input,
        scale,
        frames,
        over_ui: false,
        hover_item: 0,
    };

    // Ширину задаёт содержимое: самая длинная подпись или заголовок из
    // `Cargo.toml`, чей размер заранее неизвестен. Шире экрана при этом
    // панель не становится.
    let pad2 = (PAD * 2.0 * scale).round();
    let toggle_gap = (TOGGLE + PAD) * scale;
    let longest = LABELS
        .iter()
        .map(|label| layout.painter.measure(label) + toggle_gap)
        .fold(layout.painter.measure(TITLE), f32::max);
    let panel_w = (longest + pad2)
        .max(PANEL_MIN_W * scale)
        .min(screen.0 - (SCREEN_MARGIN * 2.0 * scale))
        .round();
    let x = ((screen.0 - panel_w) * 0.5).floor();
    let mut y = (8.0 * scale).round();

    // Стрелка сворачивания.
    let arrow = Rect {
        x: ((screen.0 - ARROW_W * scale) * 0.5).floor(),
        y,
        w: (ARROW_W * scale).round(),
        h: (ARROW_H * scale).round(),
    };
    if layout.button(arrow, "", false) {
        ui.expanded = !ui.expanded;
    }
    // Стрелок в шрифте игры нет, поэтому уголок лежит в атласе своей
    // картинкой — пиксельной, как остальная графика.
    let chevron = (CHEVRON * scale).round();
    layout.painter.stretch(
        if ui.expanded {
            icons::CHEVRON_UP
        } else {
            icons::CHEVRON_DOWN
        },
        (arrow.x + (arrow.w - chevron) * 0.5).round(),
        (arrow.y + (arrow.h - chevron) * 0.5).round(),
        chevron,
        chevron,
        colors::TEXT,
    );
    y += arrow.h + GAP * scale;

    if !ui.expanded {
        if own_cursor {
            layout.draw_cursor();
        }
        ui.search_focus = false;
        return Frame {
            over_ui: layout.over_ui,
            hover_item: 0,
            typing: false,
        };
    }

    let row_h = (ROW_H * scale).round();
    let row_gap = (GAP * 0.5 * scale).round();
    // Ячейки берём размером ровно с инвентарные: `52 * Main.inventoryScale`,
    // и без нашего уплотнения — иначе иконки не совпадут с игровыми.
    let slot = (super::SLOT_TEXTURE_SIZE * super::INVENTORY_SCALE * ui_scale).round();
    let pad = (PAD * scale).round();
    // Заголовок, шесть строк, полка зелий и ряд вкладок.
    let main_h = pad * 2.0 + row_h + (row_h + row_gap) * 6.0 + slot + GAP * scale + row_h;
    let main = Rect {
        x,
        y,
        w: panel_w,
        h: main_h,
    };
    layout.panel(main);

    let inner_x = main.x + pad;
    let inner_w = main.w - pad * 2.0;
    let mut cursor = main.y + pad;

    // Заголовок в три цвета: название, версия и автор читаются по отдельности.
    // Цвета — из палитры редкости предметов, чтобы не выдумывать свои.
    let mut pen = inner_x;
    for (part, color) in [
        (NAME, colors::TITLE),
        (VERSION, colors::RARE_BLUE),
        (AUTHOR, colors::RARE_PURPLE),
    ] {
        layout.painter.text_left(pen, cursor, row_h, part, color);
        pen += layout.painter.measure(part);
    }
    cursor += row_h;

    let next_row = |cursor: &mut f32| -> Rect {
        let r = Rect {
            x: inner_x,
            y: *cursor,
            w: inner_w,
            h: row_h,
        };
        *cursor += row_h + row_gap;
        r
    };

    let (auto_fish, quick_stack, auto_potions, enemies, cast, aim, free, potions) =
        state::with(|s| {
            (
                s.auto_fish,
                s.quick_stack,
                s.auto_potions,
                s.pull_enemy_spawns,
                s.status.bobber_cast,
                s.status.aim,
                s.status.free_slots,
                s.potions,
            )
        })
        .unwrap_or((false, true, false, false, false, None, -1, [false; 3]));

    // Точка заброса важна настолько, что выносится прямо в подпись:
    // пока она не запомнена, автомат ничего не делает и молча ждёт.
    let r = next_row(&mut cursor);
    let (note, note_color) = match (auto_fish, aim) {
        (false, _) => (String::new(), colors::TEXT),
        (true, None) => (
            " (жду первого броска удочки)".to_string(),
            colors::RARE_GREEN,
        ),
        (true, Some((ax, ay))) => (format!(" (зафиксировано {ax}:{ay})"), colors::RARE_ORANGE),
    };
    if layout.switch_row_note(r, "Авторыбалка", &note, note_color, auto_fish) {
        state::with(|s| {
            s.auto_fish = !s.auto_fish;
            s.dirty = true;
        });
    }

    let r = next_row(&mut cursor);
    if layout.switch_row(r, "Сундуки разложить при заполнении", quick_stack)
    {
        state::with(|s| {
            s.quick_stack = !s.quick_stack;
            s.dirty = true;
        });
    }

    let r = next_row(&mut cursor);
    if layout.switch_row(r, "Подсекать врагов (Герцог Рыброн)", enemies)
    {
        state::with(|s| {
            s.pull_enemy_spawns = !s.pull_enemy_spawns;
            s.dirty = true;
        });
    }

    // --- статус поплавка ---------------------------------------------------
    // Та же звёздочка, что у переключателей, но кликать по ней нечего.
    let r = next_row(&mut cursor);
    layout.row_bg(r);
    layout
        .painter
        .text_left(r.x + pad, r.y, r.h, "Поплавок", colors::TEXT);
    let label = if cast { "Заброшен" } else { "Нет" };
    let label_w = layout.painter.measure(label);
    let size = (TOGGLE * scale).round();
    layout.painter.stretch(
        if cast {
            icons::TOGGLE_ON
        } else {
            icons::TOGGLE_OFF
        },
        (r.x + r.w - pad - label_w - size - GAP * scale).round(),
        (r.y + (r.h - size) * 0.5).round(),
        size,
        size,
        colors::PLAIN,
    );
    layout.painter.text_right(
        r.x,
        r.y,
        r.w - pad,
        r.h,
        label,
        if cast { colors::ON } else { colors::MUTED },
    );

    // --- свободные ячейки --------------------------------------------------
    let r = next_row(&mut cursor);
    let value = if free < 0 {
        "?".to_string()
    } else {
        free.to_string()
    };
    layout.value_row(r, "Свободные ячейки", &value, colors::VALUE);

    // --- автопитьё ---------------------------------------------------------
    let r = next_row(&mut cursor);
    if layout.switch_row(r, "Автопитьё зелий", auto_potions) {
        state::with(|s| {
            s.auto_potions = !s.auto_potions;
            s.dirty = true;
        });
    }

    // --- ячейки зелий ------------------------------------------------------
    layout.painter.text_left(
        inner_x,
        cursor,
        slot,
        "Зелья для автоиспользования:",
        colors::TEXT,
    );
    let mut slot_x = inner_x + inner_w - slot * 3.0 - GAP * 2.0 * scale;
    for (index, (item, _, _)) in crate::game::POTIONS.iter().enumerate() {
        let cell = Rect {
            x: slot_x,
            y: cursor,
            w: slot,
            h: slot,
        };
        let on = potions[index];
        if layout.hit_item(cell, *item) {
            state::with(|s| {
                s.potions[index] = !s.potions[index];
                s.dirty = true;
            });
        }
        // Выбранное зелье помечается той же светлой рамкой, что и предмет,
        // который берём: одно правило на весь интерфейс.
        if on {
            layout.item_cell(cell, *item, Mark::Allow);
        } else {
            layout.painter.stretch(
                icons::SLOT,
                cell.x,
                cell.y,
                cell.w,
                cell.h,
                colors::SLOT_OFF,
            );
            layout
                .painter
                .icon(*item, cell.x, cell.y, cell.w, cell.h, colors::ICON_OFF);
        }
        slot_x += slot + GAP * scale;
    }
    cursor += slot + GAP * scale;

    // --- вкладки -----------------------------------------------------------
    let tab_w = ((inner_w - GAP * scale) * 0.5).floor();
    let filter_tab = Rect {
        x: inner_x,
        y: cursor,
        w: tab_w,
        h: row_h,
    };
    let stats_tab = Rect {
        x: inner_x + inner_w - tab_w,
        y: cursor,
        w: tab_w,
        h: row_h,
    };
    if layout.button(filter_tab, "Фильтр", ui.tab == Tab::Filter) {
        ui.tab = if ui.tab == Tab::Filter {
            Tab::None
        } else {
            Tab::Filter
        };
    }
    if layout.button(stats_tab, "Статистика", ui.tab == Tab::Stats) {
        ui.tab = if ui.tab == Tab::Stats {
            Tab::None
        } else {
            Tab::Stats
        };
    }

    let below = main.y + main.h + GAP * 2.0 * scale;
    match ui.tab {
        Tab::Filter => filter_window(&mut layout, ui, x, below, panel_w, screen.1, scale, slot),
        Tab::Stats => stats_window(&mut layout, x, below, panel_w, scale),
        Tab::None => {}
    }

    if own_cursor {
        layout.draw_cursor();
    }
    // Курсор в строке поиска имеет смысл только при открытом фильтре.
    if ui.tab != Tab::Filter {
        ui.search_focus = false;
    }
    Frame {
        over_ui: layout.over_ui,
        hover_item: layout.hover_item,
        typing: ui.search_focus,
    }
}

/// Окно фильтра ровно под основным и той же ширины. Колонок столько,
/// сколько влезает, остаток уходит в поля, чтобы сетка стояла по центру;
/// строки, не поместившиеся в экран, — под прокрутку.
#[allow(clippy::too_many_arguments)]
fn filter_window(
    layout: &mut Layout,
    ui: &mut UiState,
    x: f32,
    y: f32,
    panel_w: f32,
    screen_h: f32,
    scale: f32,
    cell: f32,
) {
    // Поиск по именам: их спрашивает у игры рабочий поток при подключении.
    let query = ui.search.trim().to_lowercase();
    let items: Vec<i32> = state::with(|s| {
        if query.is_empty() {
            return s.fishable.clone();
        }
        s.fishable
            .iter()
            .filter(|id| {
                s.names
                    .iter()
                    .find(|(name_id, _)| name_id == *id)
                    .is_some_and(|(_, name)| name.contains(&query))
            })
            .copied()
            .collect()
    })
    .unwrap_or_default();

    let pad = (PAD * scale).round();
    let gap = (GAP * scale).round();
    let row_h = (ROW_H * scale).round();
    let margin = (SCREEN_MARGIN * scale).round();

    // Сколько строк вообще можно показать, чтобы окно влезло в экран.
    // Шапка: заголовок, строка режима и строка поиска.
    let head_h = pad + row_h * 3.0 + gap * 2.0;
    let available = (screen_h - y - margin - head_h - pad).max(cell);
    let fits = ((available + gap) / (cell + gap)).floor().max(1.0) as usize;

    // Колонки считаем дважды: полоса прокрутки съедает ширину, но нужна она
    // только если строки не влезли — иначе сетка уехала бы влево без причины.
    let columns = |reserved: f32| {
        let grid = panel_w - pad * 2.0 - reserved;
        (((grid + gap) / (cell + gap)).floor().max(1.0)) as usize
    };
    let mut cols = columns(0.0);
    let mut rows = items.len().div_ceil(cols).max(1);
    let mut bar = 0.0;
    if rows > fits {
        bar = (BAR_W * scale).round() + gap;
        cols = columns(bar);
        rows = items.len().div_ceil(cols).max(1);
    }
    let visible = fits.min(rows);
    let scrollable = rows > visible;
    let limit = rows.saturating_sub(visible);

    let panel = Rect {
        x,
        y,
        w: panel_w,
        h: head_h + visible as f32 * (cell + gap) - gap + pad,
    };

    // Прокрутку двигаем до раскладки, иначе колесо отставало бы на кадр.
    if scrollable && layout.input.wheel != 0 && layout.hovered(panel) {
        let row = ui.filter_row as i32 - layout.input.wheel;
        ui.filter_row = row.clamp(0, limit as i32) as usize;
    }
    ui.filter_row = ui.filter_row.min(limit);

    layout.panel(panel);

    let inner_x = panel.x + pad;
    let inner_w = panel.w - pad * 2.0;
    let mut cursor = panel.y + pad;
    layout
        .painter
        .text_left(inner_x, cursor, row_h, "Фильтр", colors::TITLE);
    cursor += row_h;

    // Режим списка: белый — берём только отмеченное, чёрный — всё кроме.
    let whitelist = state::with(|s| s.whitelist_mode).unwrap_or(false);
    let r = Rect {
        x: inner_x,
        y: cursor,
        w: inner_w,
        h: row_h,
    };
    let label = if whitelist {
        "Список: белый — беру только отмеченное зелёным"
    } else {
        "Список: чёрный — беру всё, кроме отмеченного красным"
    };
    if layout.switch_row(r, label, whitelist) {
        state::with(|s| {
            s.whitelist_mode = !s.whitelist_mode;
            s.dirty = true;
        });
    }
    cursor += row_h + gap;

    // --- строка поиска -----------------------------------------------------
    let search = Rect {
        x: inner_x,
        y: cursor,
        w: inner_w,
        h: row_h,
    };
    search_field(layout, ui, search, scale);
    cursor += row_h + gap;

    // Сетку центрируем: остаток от деления уходит в поля.
    let used = cols as f32 * (cell + gap) - gap;
    let start = inner_x + ((inner_w - bar - used) * 0.5).floor().max(0.0);
    let first = ui.filter_row * cols;
    let last = (first + visible * cols).min(items.len());

    for (offset, item) in items[first..last].iter().enumerate() {
        let col = offset % cols;
        let row = offset / cols;
        let r = Rect {
            x: start + col as f32 * (cell + gap),
            y: cursor + row as f32 * (cell + gap),
            w: cell,
            h: cell,
        };
        let mark = state::with(|s| s.filter.get(item).copied().unwrap_or(Mark::Neutral))
            .unwrap_or(Mark::Neutral);
        if layout.hit_item(r, *item) {
            state::with(|s| {
                let next = s.filter.get(item).copied().unwrap_or(Mark::Neutral).next();
                if next == Mark::Neutral {
                    s.filter.remove(item);
                } else {
                    s.filter.insert(*item, next);
                }
                s.dirty = true;
            });
        }
        layout.item_cell(r, *item, mark);
    }

    if scrollable {
        let track = Rect {
            x: panel.x + panel.w - pad - (BAR_W * scale).round(),
            y: cursor,
            w: (BAR_W * scale).round(),
            h: visible as f32 * (cell + gap) - gap,
        };
        scrollbar(layout, track, ui, rows, visible, scale);
    } else {
        ui.filter_row = 0;
    }
}

/// Строка поиска, устроенная как в бестиарии и меню дублирования: слева
/// отдельная кнопка со значком, справа от неё поле. Обе берут наведение и
/// фокус золотой рамкой — это готовые картинки игры, `Button_Search_Border`
/// и `Button_Wide_Border`. Сам ввод разбирает игра, см. `input::edit_text`.
fn search_field(layout: &mut Layout, ui: &mut UiState, r: Rect, scale: f32) {
    let gap = (GAP * 0.5 * scale).round();
    let button = Rect {
        x: r.x,
        y: r.y,
        w: r.h,
        h: r.h,
    };
    let field = Rect {
        x: r.x + button.w + gap,
        y: r.y,
        w: r.w - button.w - gap,
        h: r.h,
    };

    // --- кнопка со значком -------------------------------------------------
    layout.row_bg(button);
    let pad = (PAD * 0.4 * scale).round();
    let icon = (button.h - pad * 2.0).round();
    layout.painter.stretch(
        icons::SEARCH,
        button.x + pad,
        button.y + pad,
        icon,
        icon,
        colors::PLAIN,
    );
    let over_button = layout.hovered(button);
    if layout.hit(button) {
        ui.search_focus = true;
    }
    if over_button || ui.search_focus {
        layout.frame(button, icons::FRAME_SMALL);
    }

    // --- само поле ---------------------------------------------------------
    layout.row_bg(field);
    if layout.input.clicked && layout.hovered(field) {
        ui.search_focus = true;
    }
    let _ = layout.hit(field);
    if ui.search_focus || layout.hovered(field) {
        layout.frame(field, icons::FRAME_WIDE);
    }

    // Крестик стирает набранное; появляется, только когда есть что стирать.
    let mut text_w = field.w - pad * 2.0;
    if !ui.search.is_empty() {
        let cancel = Rect {
            x: field.x + field.w - icon - pad,
            y: field.y + pad,
            w: icon,
            h: icon,
        };
        let hovered = layout.hovered(cancel);
        if layout.hit(cancel) {
            ui.search.clear();
            ui.filter_row = 0;
        }
        layout.painter.stretch(
            icons::SEARCH_CANCEL,
            cancel.x,
            cancel.y,
            cancel.w,
            cancel.h,
            if hovered {
                colors::PLAIN
            } else {
                colors::MUTED
            },
        );
        text_w -= icon + pad;
    }

    // Подсказка стоит, пока не начали набирать, — и при курсоре в строке тоже.
    let text_x = field.x + pad * 2.0;
    let mut pen = text_x;
    if ui.search.is_empty() {
        layout
            .painter
            .text_left(pen, field.y, field.h, SEARCH_HINT, colors::MUTED);
        pen += layout.painter.measure(SEARCH_HINT);
    } else {
        // Длинную строку показываем хвостом: набирают-то в конце.
        let mut shown = ui.search.as_str();
        while layout.painter.measure(shown) > text_w && !shown.is_empty() {
            let cut = shown.char_indices().nth(1).map(|(i, _)| i).unwrap_or(0);
            shown = &shown[cut..];
        }
        layout
            .painter
            .text_left(pen, field.y, field.h, shown, colors::TEXT);
        pen += layout.painter.measure(shown);
    }

    // Мигающая палочка: игра моргает своей от `Main.textBlinkerState`,
    // но до неё тянуться незачем — период тот же, на глаз не отличить.
    if ui.search_focus && layout.blink() {
        let line = (field.h * 0.55).round();
        layout.painter.rect(
            (pen + 2.0 * scale).round(),
            (field.y + (field.h - line) * 0.5).round(),
            (2.0 * scale).round().max(1.0),
            line,
            colors::TEXT,
        );
    }
}

/// Полоса прокрутки как у игры: ползунок таскается мышью, клик мимо него
/// листает страницу. Колесо обрабатывается выше, по всему окну фильтра.
fn scrollbar(
    layout: &mut Layout,
    track: Rect,
    ui: &mut UiState,
    rows: usize,
    visible: usize,
    scale: f32,
) {
    layout.painter.nine_slice(
        icons::BAR_TRACK,
        track.x,
        track.y,
        track.w,
        track.h,
        icons::BAR_INSET,
        colors::PLAIN,
    );

    let span = (rows - visible) as f32;
    let handle_h = (track.h * visible as f32 / rows as f32).max(BAR_W * scale);
    let travel = (track.h - handle_h).max(1.0);
    let handle_y = track.y + travel * ui.filter_row as f32 / span;
    let handle = Rect {
        x: track.x,
        y: handle_y,
        w: track.w,
        h: handle_h,
    };

    // Захват ползунка: запоминаем, за какое место его взяли, и тянем,
    // пока кнопка держится. Отпустили — отпустили, даже если курсор ушёл.
    if layout.input.clicked && layout.hovered(handle) {
        ui.drag = Some(layout.input.y - handle.y);
    }
    if !layout.input.down {
        ui.drag = None;
    }
    if let Some(grab) = ui.drag {
        layout.over_ui = true;
        let offset = ((layout.input.y - grab - track.y) / travel).clamp(0.0, 1.0);
        ui.filter_row = (offset * span).round() as usize;
    } else if layout.hit(track) {
        // Мимо ползунка — листаем страницами, как в списках игры.
        if layout.input.y < handle.y {
            ui.filter_row = ui.filter_row.saturating_sub(visible);
        } else if layout.input.y >= handle.y + handle.h {
            ui.filter_row = (ui.filter_row + visible).min(rows - visible);
        }
    }

    layout.painter.nine_slice(
        icons::BAR_HANDLE,
        handle.x,
        handle.y,
        handle.w,
        handle.h,
        icons::BAR_INSET,
        if ui.drag.is_some() || layout.hovered(handle) {
            colors::HANDLE_HOVER
        } else {
            colors::HANDLE
        },
    );
}

fn stats_window(layout: &mut Layout, x: f32, y: f32, w: f32, scale: f32) {
    let rows: Vec<(String, String)> = state::with(|s| {
        let t = s.stats.seconds;
        vec![
            (
                "Время рыбалки".to_string(),
                format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60),
            ),
            ("Поймано предметов".to_string(), s.stats.caught.to_string()),
            ("Поймано ящиков".to_string(), s.stats.crates.to_string()),
            (
                "Пропущено по фильтру".to_string(),
                s.stats.skipped.to_string(),
            ),
            (
                "Среднее время поклёвки".to_string(),
                format!("{:.1} сек.", s.stats.average_bite),
            ),
            ("Зелья выпито".to_string(), s.stats.potions.to_string()),
        ]
    })
    .unwrap_or_default();

    let pad = (PAD * scale).round();
    let row_h = (ROW_H * scale).round();
    let row_gap = (GAP * 0.5 * scale).round();
    let panel = Rect {
        x,
        y,
        w,
        h: pad * 2.0 + row_h + rows.len() as f32 * (row_h + row_gap) - row_gap,
    };
    layout.panel(panel);

    let inner_x = panel.x + pad;
    let inner_w = panel.w - pad * 2.0;
    let mut cursor = panel.y + pad;
    layout
        .painter
        .text(inner_x, cursor, "Статистика", colors::TITLE);
    cursor += row_h;

    for (label, value) in rows {
        let r = Rect {
            x: inner_x,
            y: cursor,
            w: inner_w,
            h: row_h,
        };
        layout.value_row(r, &label, &value, colors::VALUE);
        cursor += row_h + row_gap;
    }
}
