//! Раскладка интерфейса, попадания мыши и построение списка отрисовки.
//!
//! Режим непосредственный: каждый кадр заново считаем геометрию и тут же
//! проверяем попадания. Состояние — только «раскрыто» и «какая вкладка»,
//! всё остальное живёт в `state::Shared`.

use super::state::{self, Mark};
use super::{Painter, colors};

/// Базовые размеры при масштабе 1.0; всё остальное — умножением.
const ROW_H: f32 = 34.0;
const PANEL_W: f32 = 560.0;
const PAD: f32 = 10.0;
const GAP: f32 = 6.0;
const ARROW_W: f32 = 64.0;
const ARROW_H: f32 = 26.0;
const TOGGLE_W: f32 = 84.0;
const TOGGLE_H: f32 = 24.0;
const SLOT: f32 = 46.0;
const ICON_CELL: f32 = 46.0;
const FILTER_COLS: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    None,
    Filter,
    Stats,
}

pub struct UiState {
    pub expanded: bool,
    pub tab: Tab,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            expanded: true,
            tab: Tab::None,
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

    fn panel(&mut self, r: Rect) {
        self.painter
            .rect(r.x - 2.0, r.y - 2.0, r.w + 4.0, r.h + 4.0, colors::BORDER);
        self.painter
            .rect(r.x - 1.0, r.y - 1.0, r.w + 2.0, r.h + 2.0, colors::FRAME);
        self.painter.rect(r.x, r.y, r.w, r.h, colors::BACK);
    }

    /// Строка с подложкой, как в инвентаре игры.
    fn row_bg(&mut self, r: Rect) {
        self.painter.rect(r.x, r.y, r.w, r.h, colors::ROW_BORDER);
        self.painter
            .rect(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0, colors::ROW);
    }

    fn toggle(&mut self, r: Rect, on: bool) -> bool {
        let clicked = self.hit(r);
        self.painter.rect(r.x, r.y, r.w, r.h, colors::BORDER);
        let half = r.w * 0.62;
        if on {
            self.painter
                .rect(r.x + 1.0, r.y + 1.0, half, r.h - 2.0, colors::ON);
            self.painter.rect(
                r.x + half + 1.0,
                r.y + 1.0,
                r.w - half - 2.0,
                r.h - 2.0,
                colors::KNOB,
            );
            self.painter
                .text_centered(r.x + 1.0, r.y, half, r.h, "Вкл", colors::ON_TEXT);
        } else {
            self.painter.rect(
                r.x + 1.0,
                r.y + 1.0,
                r.w - half - 2.0,
                r.h - 2.0,
                colors::KNOB,
            );
            self.painter.rect(
                r.x + r.w - half - 1.0,
                r.y + 1.0,
                half,
                r.h - 2.0,
                colors::OFF,
            );
            self.painter.text_centered(
                r.x + r.w - half - 1.0,
                r.y,
                half,
                r.h,
                "Выкл",
                colors::OFF_TEXT,
            );
        }
        clicked
    }

    /// Кольцо статуса: рисуем четырьмя полосками, круг из квадов не собрать.
    fn ring(&mut self, x: f32, y: f32, size: f32, color: u32) {
        let t = (size * 0.18).max(1.0);
        self.painter.rect(x, y, size, t, color);
        self.painter.rect(x, y + size - t, size, t, color);
        self.painter.rect(x, y + t, t, size - t * 2.0, color);
        self.painter
            .rect(x + size - t, y + t, t, size - t * 2.0, color);
    }

    fn button(&mut self, r: Rect, label: &str, active: bool) -> bool {
        let clicked = self.hit(r);
        let hovered = r.contains(self.input.x, self.input.y);
        let fill = if active {
            colors::TAB_ACTIVE
        } else if hovered {
            colors::TAB_HOVER
        } else {
            colors::TAB
        };
        self.painter.rect(r.x, r.y, r.w, r.h, colors::BORDER);
        self.painter
            .rect(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0, fill);
        self.painter
            .text_centered(r.x, r.y, r.w, r.h, label, colors::TEXT);
        clicked
    }
}

/// Строит кадр интерфейса. Возвращает `true`, если курсор над окнами —
/// тогда клик не должен уходить в игру.
pub fn build(painter: &mut Painter, ui: &mut UiState, input: Input, screen: (f32, f32)) -> bool {
    let scale = (screen.1 / 1080.0).clamp(0.65, 2.5);
    let mut layout = Layout {
        painter,
        input,
        over_ui: false,
    };
    layout.painter.scale = scale;

    let panel_w = PANEL_W * scale;
    let x = ((screen.0 - panel_w) * 0.5).floor();
    let mut y = 8.0 * scale;

    // Стрелка сворачивания.
    let arrow = Rect {
        x: (screen.0 - ARROW_W * scale) * 0.5,
        y,
        w: ARROW_W * scale,
        h: ARROW_H * scale,
    };
    if layout.button(
        arrow,
        if ui.expanded { "\u{25B2}" } else { "\u{25BC}" },
        false,
    ) {
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
        return layout.over_ui;
    }

    let rows = 5.0;
    let main_h = PAD * 2.0 * scale
        + ROW_H * scale
        + (ROW_H + GAP * 0.5) * scale * rows
        + (SLOT + GAP) * scale
        + (ROW_H + GAP) * scale;
    let main = Rect {
        x,
        y,
        w: panel_w,
        h: main_h,
    };
    layout.panel(main);

    let inner_x = main.x + PAD * scale;
    let inner_w = main.w - PAD * 2.0 * scale;
    let mut cursor = main.y + PAD * scale;

    layout
        .painter
        .text(inner_x, cursor, "Terraria Auto Fisher", colors::TITLE);
    cursor += ROW_H * scale;

    let row_h = ROW_H * scale;
    let toggle_w = TOGGLE_W * scale;
    let toggle_h = TOGGLE_H * scale;

    let row = |layout: &mut Layout, cursor: &mut f32| -> Rect {
        let r = Rect {
            x: inner_x,
            y: *cursor,
            w: inner_w,
            h: row_h,
        };
        layout.row_bg(r);
        *cursor += row_h + GAP * 0.5 * scale;
        r
    };

    // --- переключатели -----------------------------------------------------
    let (auto_fish, quick_stack, auto_potions, cast, free, potions) = state::with(|s| {
        (
            s.auto_fish,
            s.quick_stack,
            s.auto_potions,
            s.status.bobber_cast,
            s.status.free_slots,
            s.potions,
        )
    })
    .unwrap_or((false, true, false, false, -1, [false; 3]));

    let r = row(&mut layout, &mut cursor);
    layout.painter.text(
        r.x + PAD * scale,
        r.y + (r.h - line(scale)) * 0.5,
        "Авторыбалка",
        colors::TEXT,
    );
    let knob = Rect {
        x: r.x + r.w - toggle_w - PAD * scale,
        y: r.y + (r.h - toggle_h) * 0.5,
        w: toggle_w,
        h: toggle_h,
    };
    if layout.toggle(knob, auto_fish) {
        state::with(|s| {
            s.auto_fish = !s.auto_fish;
            s.dirty = true;
        });
    }

    let r = row(&mut layout, &mut cursor);
    layout.painter.text(
        r.x + PAD * scale,
        r.y + (r.h - line(scale)) * 0.5,
        "Сундуки разложить при заполнении",
        colors::TEXT,
    );
    let knob = Rect {
        x: r.x + r.w - toggle_w - PAD * scale,
        y: r.y + (r.h - toggle_h) * 0.5,
        w: toggle_w,
        h: toggle_h,
    };
    if layout.toggle(knob, quick_stack) {
        state::with(|s| {
            s.quick_stack = !s.quick_stack;
            s.dirty = true;
        });
    }

    // --- статус поплавка ---------------------------------------------------
    let r = row(&mut layout, &mut cursor);
    layout.painter.text(
        r.x + PAD * scale,
        r.y + (r.h - line(scale)) * 0.5,
        "Поплавок",
        colors::TEXT,
    );
    let size = 14.0 * scale;
    let ring_x = r.x + r.w - PAD * scale - 110.0 * scale;
    layout.ring(
        ring_x,
        r.y + (r.h - size) * 0.5,
        size,
        if cast { colors::ON } else { colors::OFF },
    );
    layout.painter.text(
        ring_x + size + 8.0 * scale,
        r.y + (r.h - line(scale)) * 0.5,
        if cast { "Заброшен" } else { "Нет" },
        if cast { colors::ON_TEXT } else { colors::MUTED },
    );

    // --- свободные ячейки --------------------------------------------------
    let r = row(&mut layout, &mut cursor);
    layout.painter.text(
        r.x + PAD * scale,
        r.y + (r.h - line(scale)) * 0.5,
        "Свободные ячейки",
        colors::TEXT,
    );
    let value = if free < 0 {
        "?".to_string()
    } else {
        free.to_string()
    };
    layout.painter.text_right(
        r.x,
        r.y + (r.h - line(scale)) * 0.5,
        r.w - PAD * scale,
        &value,
        colors::VALUE,
    );

    // --- автопитьё ---------------------------------------------------------
    let r = row(&mut layout, &mut cursor);
    layout.painter.text(
        r.x + PAD * scale,
        r.y + (r.h - line(scale)) * 0.5,
        "АвтоПитьё зелий",
        colors::TEXT,
    );
    let knob = Rect {
        x: r.x + r.w - toggle_w - PAD * scale,
        y: r.y + (r.h - toggle_h) * 0.5,
        w: toggle_w,
        h: toggle_h,
    };
    if layout.toggle(knob, auto_potions) {
        state::with(|s| {
            s.auto_potions = !s.auto_potions;
            s.dirty = true;
        });
    }

    // --- ячейки зелий ------------------------------------------------------
    let slot = SLOT * scale;
    layout.painter.text(
        inner_x,
        cursor + (slot - line(scale)) * 0.5,
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
        layout.painter.rect(
            cell.x,
            cell.y,
            cell.w,
            cell.h,
            if on {
                colors::SLOT_ON_BORDER
            } else {
                colors::BORDER
            },
        );
        layout.painter.rect(
            cell.x + 2.0,
            cell.y + 2.0,
            cell.w - 4.0,
            cell.h - 4.0,
            if on { colors::SLOT_ON } else { colors::SLOT },
        );
        layout.painter.icon(*item, cell.x, cell.y, cell.w, cell.h);
        slot_x += slot + GAP * scale;
    }
    cursor += slot + GAP * scale;

    // --- вкладки -----------------------------------------------------------
    let tab_w = (inner_w - GAP * scale) * 0.5;
    let filter_tab = Rect {
        x: inner_x,
        y: cursor,
        w: tab_w,
        h: row_h,
    };
    let stats_tab = Rect {
        x: inner_x + tab_w + GAP * scale,
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
        Tab::Filter => filter_window(&mut layout, x, below, panel_w, scale),
        Tab::Stats => stats_window(&mut layout, x, below, panel_w, scale),
        Tab::None => {}
    }

    layout.over_ui
}

fn line(scale: f32) -> f32 {
    20.0 * scale
}

fn filter_window(layout: &mut Layout, x: f32, y: f32, w: f32, scale: f32) {
    let items = state::with(|s| s.fishable.clone()).unwrap_or_default();
    let cell = ICON_CELL * scale;
    let cols = FILTER_COLS;
    let rows = items.len().div_ceil(cols).max(1);
    let grid_h = rows as f32 * (cell + GAP * scale);
    let panel = Rect {
        x,
        y,
        w,
        h: PAD * 2.0 * scale + ROW_H * scale * 2.0 + grid_h,
    };
    layout.panel(panel);

    let inner_x = panel.x + PAD * scale;
    let mut cursor = panel.y + PAD * scale;
    layout
        .painter
        .text(inner_x, cursor, "Фильтр", colors::TITLE);
    cursor += ROW_H * scale;

    // Переключатель режима списка.
    let whitelist = state::with(|s| s.whitelist_mode).unwrap_or(false);
    let r = Rect {
        x: inner_x,
        y: cursor,
        w: panel.w - PAD * 2.0 * scale,
        h: ROW_H * scale,
    };
    layout.row_bg(r);
    layout.painter.text(
        r.x + PAD * scale,
        r.y + (r.h - line(scale)) * 0.5,
        "Список",
        colors::TEXT,
    );
    let knob = Rect {
        x: r.x + r.w - TOGGLE_W * scale - PAD * scale,
        y: r.y + (r.h - TOGGLE_H * scale) * 0.5,
        w: TOGGLE_W * scale,
        h: TOGGLE_H * scale,
    };
    let clicked = layout.hit(knob);
    layout
        .painter
        .rect(knob.x, knob.y, knob.w, knob.h, colors::BORDER);
    layout.painter.rect(
        knob.x + 1.0,
        knob.y + 1.0,
        knob.w - 2.0,
        knob.h - 2.0,
        if whitelist { colors::ON } else { colors::OFF },
    );
    layout.painter.text_centered(
        knob.x,
        knob.y,
        knob.w,
        knob.h,
        if whitelist {
            "Белый"
        } else {
            "Чёрный"
        },
        colors::TEXT,
    );
    if clicked {
        state::with(|s| {
            s.whitelist_mode = !s.whitelist_mode;
            s.dirty = true;
        });
    }
    cursor += ROW_H * scale + GAP * scale;

    // Сетка предметов.
    for (index, item) in items.iter().enumerate() {
        let col = index % cols;
        let row = index / cols;
        let grid_w = cols as f32 * (cell + GAP * scale) - GAP * scale;
        let start = panel.x + (panel.w - grid_w) * 0.5;
        let r = Rect {
            x: start + col as f32 * (cell + GAP * scale),
            y: cursor + row as f32 * (cell + GAP * scale),
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
        let border = match mark {
            Mark::Allow => colors::MARK_ALLOW,
            Mark::Deny => colors::MARK_DENY,
            Mark::Neutral => colors::BORDER,
        };
        layout.painter.rect(r.x, r.y, r.w, r.h, border);
        layout
            .painter
            .rect(r.x + 2.0, r.y + 2.0, r.w - 4.0, r.h - 4.0, colors::SLOT);
        layout.painter.icon(*item, r.x, r.y, r.w, r.h);
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

    let panel = Rect {
        x,
        y,
        w,
        h: PAD * 2.0 * scale + ROW_H * scale + rows.len() as f32 * (ROW_H + GAP * 0.5) * scale,
    };
    layout.panel(panel);

    let inner_x = panel.x + PAD * scale;
    let inner_w = panel.w - PAD * 2.0 * scale;
    let mut cursor = panel.y + PAD * scale;
    layout
        .painter
        .text(inner_x, cursor, "Статистика", colors::TITLE);
    cursor += ROW_H * scale;

    for (label, value) in rows {
        let r = Rect {
            x: inner_x,
            y: cursor,
            w: inner_w,
            h: ROW_H * scale,
        };
        layout.row_bg(r);
        layout.painter.text(
            r.x + PAD * scale,
            r.y + (r.h - line(scale)) * 0.5,
            &label,
            colors::TEXT,
        );
        layout.painter.text_right(
            r.x,
            r.y + (r.h - line(scale)) * 0.5,
            r.w - PAD * scale,
            &value,
            colors::VALUE,
        );
        cursor += ROW_H * scale + GAP * 0.5 * scale;
    }
}
