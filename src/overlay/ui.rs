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

/// Базовые размеры при масштабе 1.0; всё остальное — умножением.
const ROW_H: f32 = 34.0;
const PANEL_W: f32 = 560.0;
const PAD: f32 = 12.0;
const GAP: f32 = 6.0;
const ARROW_W: f32 = 64.0;
const ARROW_H: f32 = 26.0;
/// Переключатель рисуется в натуральную величину текстуры: 14 пикселей.
const TOGGLE: f32 = 14.0;
const SLOT: f32 = 46.0;
/// Поля от края экрана у окна фильтра, которое тянется во всю ширину.
const SCREEN_MARGIN: f32 = 24.0;
const BAR_W: f32 = 20.0;

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
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            expanded: true,
            tab: Tab::None,
            filter_row: 0,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Input {
    pub x: f32,
    pub y: f32,
    /// Кнопка нажата именно в этом кадре.
    pub clicked: bool,
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
    /// Курсор попал хоть в одну нашу область — игре клик отдавать нельзя.
    pub over_ui: bool,
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

    /// Окно: фон и обводка поверх него — ровно так рисует панели игра.
    fn panel(&mut self, r: Rect) {
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

    /// Ячейка инвентаря. Смысл слота показываем сменой текстуры, как игра:
    /// у неё под избранное и под ячейки монет свои цветные подложки.
    fn cell(&mut self, r: Rect, id: i32, tint: u32) {
        self.painter.stretch(id, r.x, r.y, r.w, r.h, tint);
    }

    /// Переключатель из меню настроек: кольцо — выключено, диск — включено.
    fn toggle(&mut self, r: Rect, on: bool) -> bool {
        let clicked = self.hit(r);
        let id = if on {
            icons::TOGGLE_ON
        } else {
            icons::TOGGLE_OFF
        };
        let tint = if self.hovered(r) {
            colors::PLAIN
        } else if on {
            colors::ON
        } else {
            colors::MUTED
        };
        self.painter.stretch(id, r.x, r.y, r.w, r.h, tint);
        clicked
    }

    /// Переключатель, прижатый к правому краю строки, вместе с подписью слева.
    fn switch_row(&mut self, r: Rect, label: &str, on: bool) -> bool {
        self.row_bg(r);
        let line = self.painter.line_height();
        self.painter.text(
            r.x + PAD * self.scale,
            r.y + (r.h - line) * 0.5,
            label,
            colors::TEXT,
        );
        let size = (TOGGLE * self.scale).round();
        let knob = Rect {
            x: r.x + r.w - size - PAD * self.scale,
            y: r.y + (r.h - size) * 0.5,
            w: size,
            h: size,
        };
        self.toggle(knob, on)
    }

    /// Строка «подпись — значение».
    fn value_row(&mut self, r: Rect, label: &str, value: &str, color: u32) {
        self.row_bg(r);
        let line = self.painter.line_height();
        let pad = PAD * self.scale;
        self.painter
            .text(r.x + pad, r.y + (r.h - line) * 0.5, label, colors::TEXT);
        self.painter
            .text_right(r.x, r.y + (r.h - line) * 0.5, r.w - pad, value, color);
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

    fn button(&mut self, r: Rect, label: &str, active: bool) -> bool {
        let clicked = self.hit(r);
        let tint = if active {
            colors::BUTTON_ACTIVE
        } else if self.hovered(r) {
            colors::BUTTON_HOVER
        } else {
            colors::BUTTON
        };
        self.painter
            .nine_slice(icons::BUTTON, r.x, r.y, r.w, r.h, icons::BUTTON_INSET, tint);
        self.painter
            .text_centered(r.x, r.y, r.w, r.h, label, colors::TEXT);
        clicked
    }
}

/// Строит кадр интерфейса. Возвращает `true`, если курсор над окнами —
/// тогда клик не должен уходить в игру.
pub fn build(
    painter: &mut Painter,
    ui: &mut UiState,
    input: Input,
    screen: (f32, f32),
    own_cursor: bool,
) -> bool {
    let scale = (screen.1 / 1080.0).clamp(0.65, 2.5);
    painter.scale = scale;
    let mut layout = Layout {
        painter,
        input,
        scale,
        over_ui: false,
    };

    let panel_w = (PANEL_W * scale).round();
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
    // Шрифт игры не содержит стрелок — рисуем треугольник сами.
    layout.painter.triangle(
        arrow.x + arrow.w * 0.5,
        arrow.y + arrow.h * 0.5,
        7.0 * scale,
        ui.expanded,
        colors::TEXT,
    );
    y += arrow.h + GAP * scale;

    if !ui.expanded {
        if own_cursor {
            layout.draw_cursor();
        }
        return layout.over_ui;
    }

    let row_h = (ROW_H * scale).round();
    let row_gap = (GAP * 0.5 * scale).round();
    let slot = (SLOT * scale).round();
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

    layout
        .painter
        .text(inner_x, cursor, "Terraria Auto Fisher", colors::TITLE);
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

    let (auto_fish, quick_stack, auto_potions, enemies, cast, free, potions) = state::with(|s| {
        (
            s.auto_fish,
            s.quick_stack,
            s.auto_potions,
            s.pull_enemy_spawns,
            s.status.bobber_cast,
            s.status.free_slots,
            s.potions,
        )
    })
    .unwrap_or((false, true, false, false, false, -1, [false; 3]));

    let r = next_row(&mut cursor);
    if layout.switch_row(r, "Авторыбалка", auto_fish) {
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
    // Тот же кружок, что у переключателей, но кликать по нему нечего.
    let r = next_row(&mut cursor);
    layout.row_bg(r);
    let line = layout.painter.line_height();
    layout.painter.text(
        r.x + pad,
        r.y + (r.h - line) * 0.5,
        "Поплавок",
        colors::TEXT,
    );
    let label = if cast { "Заброшен" } else { "Нет" };
    let label_w = layout.painter.measure(label);
    let size = (TOGGLE * scale).round();
    layout.painter.stretch(
        if cast {
            icons::TOGGLE_ON
        } else {
            icons::TOGGLE_OFF
        },
        r.x + r.w - pad - label_w - size - GAP * scale,
        r.y + (r.h - size) * 0.5,
        size,
        size,
        if cast { colors::ON } else { colors::MUTED },
    );
    layout.painter.text_right(
        r.x,
        r.y + (r.h - line) * 0.5,
        r.w - pad,
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
    layout.painter.text(
        inner_x,
        cursor + (slot - line) * 0.5,
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
        if layout.hit(cell) {
            state::with(|s| {
                s.potions[index] = !s.potions[index];
                s.dirty = true;
            });
        }
        layout.cell(
            cell,
            if on { icons::SLOT_ALLOW } else { icons::SLOT },
            if on { colors::PLAIN } else { colors::SLOT_OFF },
        );
        layout.painter.icon(
            *item,
            cell.x,
            cell.y,
            cell.w,
            cell.h,
            if on { colors::PLAIN } else { colors::ICON_OFF },
        );
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
        Tab::Filter => filter_window(&mut layout, ui, below, screen, scale),
        Tab::Stats => stats_window(&mut layout, x, below, panel_w, scale),
        Tab::None => {}
    }

    if own_cursor {
        layout.draw_cursor();
    }
    layout.over_ui
}

/// Окно фильтра во всю ширину экрана: колонок столько, сколько влезает,
/// остаток уходит в поля, чтобы сетка стояла ровно по центру.
fn filter_window(layout: &mut Layout, ui: &mut UiState, y: f32, screen: (f32, f32), scale: f32) {
    let items = state::with(|s| s.fishable.clone()).unwrap_or_default();
    let pad = (PAD * scale).round();
    let gap = (GAP * scale).round();
    let cell = (SLOT * scale).round();
    let row_h = (ROW_H * scale).round();
    let margin = (SCREEN_MARGIN * scale).round();

    let panel_w = (screen.0 - margin * 2.0).floor();
    let x = margin;
    // Сколько строк вообще можно показать, чтобы окно влезло в экран.
    let head_h = pad + row_h * 2.0 + gap;
    let available = (screen.1 - y - margin - head_h - pad).max(cell);
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

    ui.filter_row = ui.filter_row.min(rows.saturating_sub(visible));

    let panel = Rect {
        x,
        y,
        w: panel_w,
        h: head_h + visible as f32 * (cell + gap) - gap + pad,
    };
    layout.panel(panel);

    let inner_x = panel.x + pad;
    let inner_w = panel.w - pad * 2.0;
    let mut cursor = panel.y + pad;
    layout
        .painter
        .text(inner_x, cursor, "Фильтр", colors::TITLE);
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
        if layout.hit(r) {
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
        let backing = match mark {
            Mark::Allow => icons::SLOT_ALLOW,
            Mark::Deny => icons::SLOT_DENY,
            Mark::Neutral if layout.hovered(r) => icons::SLOT_HOVER,
            Mark::Neutral => icons::SLOT,
        };
        layout.cell(r, backing, colors::SLOT);
        layout
            .painter
            .icon(*item, r.x, r.y, r.w, r.h, colors::PLAIN);
    }

    if scrollable {
        let track = Rect {
            x: panel.x + panel.w - pad - (BAR_W * scale).round(),
            y: cursor,
            w: (BAR_W * scale).round(),
            h: visible as f32 * (cell + gap) - gap,
        };
        scrollbar(layout, track, ui, rows, visible, scale);
    }
}

/// Полоса прокрутки как у игры. Колеса мыши мы не видим, поэтому клик выше
/// ползунка листает страницу вверх, ниже — вниз.
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
    let travel = track.h - handle_h;
    let handle_y = track.y + travel * ui.filter_row as f32 / span;
    let handle = Rect {
        x: track.x,
        y: handle_y,
        w: track.w,
        h: handle_h,
    };
    layout.painter.nine_slice(
        icons::BAR_HANDLE,
        handle.x,
        handle.y,
        handle.w,
        handle.h,
        icons::BAR_INSET,
        if layout.hovered(handle) {
            colors::HANDLE_HOVER
        } else {
            colors::HANDLE
        },
    );

    if layout.hit(track) {
        if layout.input.y < handle.y {
            ui.filter_row = ui.filter_row.saturating_sub(visible);
        } else if layout.input.y >= handle.y + handle.h {
            ui.filter_row = (ui.filter_row + visible).min(rows - visible);
        }
    }
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
