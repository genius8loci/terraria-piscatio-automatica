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
use crate::lang;

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

/// Пара картинок переключателя и место, которое она занимает при масштабе 1.
#[derive(Clone, Copy)]
struct Knob {
    on: i32,
    off: i32,
    w: f32,
    h: f32,
}

/// Переключатель по умолчанию — звёздочка ранга из бестиария.
const STAR: Knob = Knob {
    on: icons::TOGGLE_ON,
    off: icons::TOGGLE_OFF,
    w: TOGGLE,
    h: TOGGLE,
};
/// Строка про сундуки: та же кнопка, что в инвентаре игры. Картинка 32x30,
/// поэтому и здесь не квадрат — иначе сундук сплющило бы.
const CHEST: Knob = Knob {
    on: icons::CHEST_ON,
    off: icons::CHEST_OFF,
    w: 24.0,
    h: 22.5,
};
/// Режим списка в фильтре: катушка лески, светлая под белый список
/// и притемнённая под чёрный. Картинка 30x30, место под неё квадратное.
const LIST: Knob = Knob {
    on: icons::LIST_WHITE,
    off: icons::LIST_BLACK,
    w: 22.0,
    h: 22.0,
};
/// Насколько кнопка-переключатель выше строки — с каждой стороны.
/// Столько же уходит в просвет между строкой и кнопкой.
const KNOB_OVER: f32 = 2.0;
/// Сторона уголка на кнопке сворачивания.
const CHEVRON: f32 = 16.0;
/// Подсказка в пустой строке поиска — как у игры в её собственных полях.
/// Просвет между кнопкой поиска и полем: у игры ровно три пикселя.
const SEARCH_GAP: f32 = 3.0;
/// Высота строки поиска. У игры `UIWrappedSearchBar.Height = 24`, и она
/// заметно ниже наших строк: там одна короткая надпись, а не подпись
/// с переключателем.
const SEARCH_H: f32 = 24.0;
/// Насколько текст в поле мельче остального: `new UISearchBar(text, 0.8f)`.
const SEARCH_TEXT: f32 = 0.8;
/// Сторона крестика «стереть»: `UI/SearchCancel` в натуральную величину.
const SEARCH_CANCEL_SIZE: f32 = 24.0;
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

/// Своя подсказка под курсором — не про предмет, а про то, почему ячейка
/// не нажимается. Между кадром и просьбой к игре значение живёт в атомике,
/// поэтому это код, а не строка; сам текст — в `hint_text`.
pub const HINT_NONE: u8 = 0;
pub const HINT_NO_POTION: u8 = 1;
pub const HINT_LIST_BLACK: u8 = 2;
pub const HINT_LIST_WHITE: u8 = 3;

/// Текст подсказки по её коду.
pub fn hint_text(hint: u8) -> Option<&'static str> {
    let t = lang::t();
    match hint {
        HINT_NO_POTION => Some(t.hint_no_potion),
        HINT_LIST_BLACK => Some(t.hint_list_black),
        HINT_LIST_WHITE => Some(t.hint_list_white),
        _ => None,
    }
}

/// Что кадр рассказал наружу.
#[derive(Clone, Copy, Default)]
pub struct Frame {
    /// Курсор над окном — клик не должен уходить в игру.
    pub over_ui: bool,
    /// Предмет под курсором; `0` — ничего.
    pub hover_item: i32,
    /// Своя подсказка под курсором; `HINT_NONE` — ничего.
    pub hint: u8,
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
    /// Своя подсказка под курсором, кодом; `HINT_NONE` — нет.
    pub hint: u8,
    /// В этом кадре кликнули по строке поиска. Клик мимо неё снимает фокус.
    pub clicked_search: bool,
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

    /// Строка внутри окна. Картинка — та же, что у окон и кнопок, только
    /// залитая цветом подложки: у родного `InnerPanelBackground` уголки
    /// скруглены на один пиксель, и рядом с кнопками строки выглядели
    /// вырубленными по линейке.
    fn row_bg(&mut self, r: Rect) {
        self.painter.nine_slice(
            icons::PANEL,
            r.x,
            r.y,
            r.w,
            r.h,
            icons::PANEL_INSET,
            colors::ROW,
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
            // Крестик кладём ровно так же, как игра в меню дублирования:
            // по центру ячейки и в натуральную величину картинки.
            let size = (r.w * super::CROSS_TEXTURE_SIZE / super::SLOT_TEXTURE_SIZE).round();
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

    /// Насколько кнопка-переключатель выступает за строку сверху и снизу.
    /// Столько же уходит в просвет между ними, чтобы кнопка читалась
    /// продолжением строки, а не наклейкой поверх.
    fn knob_over(&self) -> f32 {
        (KNOB_OVER * self.scale).round().max(1.0)
    }

    /// Сторона кнопки-переключателя при такой высоте строки.
    fn knob_side(&self, row_h: f32) -> f32 {
        row_h + self.knob_over() * 2.0
    }

    /// Переключатель: две картинки игры, подсвеченная и обычная. Цвет
    /// у обеих свой, поэтому красим их белым; наведение показываем
    /// золотой рамкой, как игра.
    ///
    /// Место под кнопку квадратное, а картинка в него вписывается со своими
    /// пропорциями: у сундука 32x30, у звёздочки и катушки квадрат.
    fn toggle(&mut self, place: Rect, knob: Knob, on: bool) -> bool {
        let clicked = self.hit(place);
        let id = if on { knob.on } else { knob.off };
        let fit = (place.w / knob.w).min(place.h / knob.h);
        let w = (knob.w * fit).round();
        let h = (knob.h * fit).round();
        self.painter.stretch(
            id,
            (place.x + (place.w - w) * 0.5).round(),
            (place.y + (place.h - h) * 0.5).round(),
            w,
            h,
            colors::PLAIN,
        );
        if self.hovered(place) {
            self.frame(place, icons::FRAME_SMALL);
        }
        clicked
    }

    /// Переключатель, прижатый к правому краю строки, вместе с подписью слева.
    fn switch_row(&mut self, r: Rect, label: &str, on: bool) -> bool {
        self.switch_row_note(r, label, "", colors::TEXT, STAR, on)
    }

    /// То же, но своей картинкой переключателя.
    fn switch_row_knob(&mut self, r: Rect, label: &str, knob: Knob, on: bool) -> bool {
        self.switch_row_note(r, label, "", colors::TEXT, knob, on)
    }

    /// То же, но с приписком своего цвета сразу за подписью.
    fn switch_row_note(
        &mut self,
        r: Rect,
        label: &str,
        note: &str,
        note_color: u32,
        knob: Knob,
        on: bool,
    ) -> bool {
        // Подложка строки не доходит до правого края: там стоит кнопка,
        // и между ними просвет. Кнопка выше строки на пару пикселей с каждой
        // стороны — так она читается переключателем, а не частью подписи.
        let over = self.knob_over();
        let side = self.knob_side(r.h);
        let bar = Rect {
            x: r.x,
            y: r.y,
            w: (r.w - side - over * 2.0).max(r.h),
            h: r.h,
        };
        self.row_bg(bar);
        let pad = (PAD * self.scale).round();
        self.painter
            .text_left(bar.x + pad, bar.y, bar.h, label, colors::TEXT);
        if !note.is_empty() {
            let after = bar.x + pad + self.painter.measure(label);
            self.painter
                .text_left(after, bar.y, bar.h, note, note_color);
        }
        let place = Rect {
            x: r.x + r.w - side,
            y: r.y - over,
            w: side,
            h: side,
        };
        self.toggle(place, knob, on)
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
        hint: HINT_NONE,
        clicked_search: false,
    };

    // Ширину задаёт содержимое: самая длинная подпись или заголовок из
    // `Cargo.toml`, чей размер заранее неизвестен. Шире экрана при этом
    // панель не становится.
    let pad2 = (PAD * 2.0 * scale).round();
    // Справа от подписи должно остаться место под кнопку и просвет перед ней:
    // кнопка ровно на `KNOB_OVER` выше строки с каждой стороны.
    let toggle_gap = (ROW_H + KNOB_OVER * 4.0 + PAD) * scale;
    let t = lang::t();
    // Подпись авторыбалки меряем с самым длинным припиской: иначе координаты
    // заброса налезали бы на кнопку переключателя.
    let aim_sample = lang::fill(t.note_aim, &["1920", "1080"]);
    let longest = t
        .row_labels()
        .iter()
        .map(|label| layout.painter.measure(label) + toggle_gap)
        .chain(std::iter::once(
            layout.painter.measure(t.auto_fish) + layout.painter.measure(&aim_sample) + toggle_gap,
        ))
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
            hint: HINT_NONE,
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

    let (
        auto_fish,
        quick_stack,
        auto_potions,
        enemies,
        cast,
        aim,
        recast,
        free,
        potions,
        potions_missing,
    ) = state::with(|s| {
        (
            s.auto_fish,
            s.quick_stack,
            s.auto_potions,
            s.pull_enemy_spawns,
            s.status.bobber_cast,
            s.status.aim,
            s.status.recast,
            s.status.free_slots,
            s.potions,
            s.status.potions_missing,
        )
    })
    .unwrap_or((
        false, true, false, false, false, None, false, -1, [false; 3], [false; 3],
    ));

    // Точка заброса важна настолько, что выносится прямо в подпись:
    // пока она не запомнена, автомат ничего не делает и молча ждёт.
    let r = next_row(&mut cursor);
    let (note, note_color) = match (auto_fish, aim, recast) {
        (false, _, _) => (String::new(), colors::TEXT),
        // Включились при уже заброшенном поплавке: по нему точку не взять.
        (true, _, true) => (t.note_recast.to_string(), colors::RARE_GREEN),
        (true, None, _) => (t.note_wait_cast.to_string(), colors::RARE_GREEN),
        (true, Some((ax, ay)), _) => (
            lang::fill(t.note_aim, &[&ax.to_string(), &ay.to_string()]),
            colors::RARE_ORANGE,
        ),
    };
    if layout.switch_row_note(r, t.auto_fish, &note, note_color, STAR, auto_fish) {
        state::with(|s| {
            s.auto_fish = !s.auto_fish;
            s.dirty = true;
        });
    }

    let r = next_row(&mut cursor);
    if layout.switch_row_knob(r, t.quick_stack, CHEST, quick_stack) {
        state::with(|s| {
            s.quick_stack = !s.quick_stack;
            s.dirty = true;
        });
    }

    let r = next_row(&mut cursor);
    if layout.switch_row(r, t.pull_enemies, enemies) {
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
        .text_left(r.x + pad, r.y, r.h, t.bobber, colors::TEXT);
    let label = if cast { t.bobber_cast } else { t.bobber_none };
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
    layout.value_row(r, t.free_slots, &value, colors::VALUE);

    // --- автопитьё ---------------------------------------------------------
    let r = next_row(&mut cursor);
    if layout.switch_row(r, t.auto_potions, auto_potions) {
        state::with(|s| {
            s.auto_potions = !s.auto_potions;
            s.dirty = true;
        });
    }

    // --- ячейки зелий ------------------------------------------------------
    layout
        .painter
        .text_left(inner_x, cursor, slot, t.potions_shelf, colors::TEXT);
    let mut slot_x = inner_x + inner_w - slot * 3.0 - GAP * 2.0 * scale;
    for (index, (item, _, _)) in crate::game::POTIONS.iter().enumerate() {
        let cell = Rect {
            x: slot_x,
            y: cursor,
            w: slot,
            h: slot,
        };
        let on = potions[index];
        // Зелья нет в инвентаре — ячейка не нажимается: включать питьё тем,
        // чего нет, бессмысленно. Показываем это так же, как отвергнутый
        // предмет в фильтре, и объясняем подсказкой.
        if potions_missing[index] {
            if layout.hovered(cell) {
                layout.over_ui = true;
                layout.hint = HINT_NO_POTION;
            }
            layout.item_cell(cell, *item, Mark::Deny);
            slot_x += slot + GAP * scale;
            continue;
        }
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
    if layout.button(filter_tab, t.tab_filter, ui.tab == Tab::Filter) {
        ui.tab = if ui.tab == Tab::Filter {
            Tab::None
        } else {
            Tab::Filter
        };
    }
    if layout.button(stats_tab, t.tab_stats, ui.tab == Tab::Stats) {
        ui.tab = if ui.tab == Tab::Stats {
            Tab::None
        } else {
            Tab::Stats
        };
    }

    let below = main.y + main.h + GAP * 2.0 * scale;
    match ui.tab {
        Tab::Filter => filter_window(
            &mut layout,
            ui,
            x,
            below,
            panel_w,
            screen.1,
            scale,
            ui_scale,
            slot,
        ),
        Tab::Stats => stats_window(&mut layout, x, below, panel_w, scale),
        Tab::None => {}
    }

    if own_cursor {
        layout.draw_cursor();
    }
    // Клик мимо строки поиска снимает фокус, но набранного не стирает —
    // ровно так ведёт себя поле игры.
    if input.clicked && !layout.clicked_search {
        ui.search_focus = false;
    }
    // Курсор в строке поиска имеет смысл только при открытом фильтре.
    if ui.tab != Tab::Filter {
        ui.search_focus = false;
    }
    Frame {
        over_ui: layout.over_ui,
        hover_item: layout.hover_item,
        hint: layout.hint,
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
    ui_scale: f32,
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
                s.facts(**id)
                    .is_some_and(|facts| facts.search.contains(&query))
            })
            .copied()
            .collect()
    })
    .unwrap_or_default();

    let pad = (PAD * scale).round();
    let gap = (GAP * scale).round();
    let row_h = (ROW_H * scale).round();
    // Строка поиска — единственная, что живёт по меркам самой игры, без
    // нашего уплотнения: у неё готовые картинки в натуральную величину,
    // и на нашем масштабе она выходила на пару пикселей ниже оригинала.
    let search_h = (SEARCH_H * ui_scale).round();
    let margin = (SCREEN_MARGIN * scale).round();

    // Сколько строк вообще можно показать, чтобы окно влезло в экран.
    // Шапка: заголовок с переключателем режима и строка поиска под ним.
    let head_h = pad + row_h + search_h + gap * 2.0;
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

    // Шапка: заголовок по центру и сразу за ним катушка — переключатель
    // режима списка. Отдельной строки под режим нет: что он значит,
    // рассказывает подсказка под курсором, как у ячеек зелий.
    let whitelist = state::with(|s| s.whitelist_mode).unwrap_or(false);
    let title = lang::t().tab_filter;
    let title_w = layout.painter.measure(title);
    let side = layout.knob_side(row_h);
    let head_gap = (GAP * scale).round();
    let head_x = (inner_x + (inner_w - title_w - head_gap - side) * 0.5)
        .floor()
        .max(inner_x);
    layout
        .painter
        .text_left(head_x, cursor, row_h, title, colors::TITLE);
    let knob = Rect {
        x: head_x + title_w + head_gap,
        y: (cursor + (row_h - side) * 0.5).round(),
        w: side,
        h: side,
    };
    if layout.hovered(knob) {
        layout.hint = if whitelist {
            HINT_LIST_WHITE
        } else {
            HINT_LIST_BLACK
        };
    }
    if layout.toggle(knob, LIST, whitelist) {
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
        h: search_h,
    };
    search_field(layout, ui, search, ui_scale);
    cursor += search_h + gap;

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

/// Строка поиска, собранная как у игры (`UIWrappedSearchBar`): слева кнопка
/// со значком, через три пикселя — панель поля во всю оставшуюся ширину.
/// Наведение и фокус берутся золотой рамкой — это готовые картинки игры,
/// `Button_Search_Border` и `Button_Wide_Border`. Сам ввод разбирает игра,
/// см. `input::edit_text`.
fn search_field(layout: &mut Layout, ui: &mut UiState, r: Rect, ui_scale: f32) {
    // Клик куда угодно в этой строке — «по поиску»: снаружи по нему решают,
    // снимать ли фокус.
    if layout.input.clicked && layout.hovered(r) {
        layout.clicked_search = true;
    }

    let gap = (SEARCH_GAP * ui_scale).round();
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
    // Своей подложки под неё не кладём: `Button_Search` — это уже готовая
    // кнопка вместе с тёмным скруглённым фоном, и вторая коробка под ней
    // выглядела значком, забытым в ячейке.
    let pad = (PAD * 0.4 * ui_scale).round();
    layout.painter.stretch(
        icons::SEARCH,
        button.x,
        button.y,
        button.w,
        button.h,
        colors::PLAIN,
    );
    let over_button = layout.hovered(button);
    // Значок переключает фокус, а не только ставит: второй щелчок по нему
    // убирает курсор ввода. Это `Click_SearchArea` -> `ToggleTakingText`.
    if layout.hit(button) {
        ui.search_focus = !ui.search_focus;
    }
    // Рамка только под курсором: это `SetHoverImage`, а не отметка фокуса.
    if over_button {
        layout.frame(button, icons::FRAME_SMALL);
    }

    // --- само поле ---------------------------------------------------------
    // Попадание считаем до отрисовки: от фокуса зависит цвет обводки.
    if layout.hit(field) {
        ui.search_focus = true;
    }
    // Заливка и обводка одного цвета — так игра рисует `_searchBoxPanel`.
    // Фокус она показывает не картинкой поверх, а перекраской этой же
    // обводки в `Main.OurFavoriteColor`; своя рамка ложилась мимо, потому
    // что скруглена мельче, чем панель под ней.
    let border = if ui.search_focus {
        colors::FOCUS
    } else {
        colors::SEARCH_FIELD
    };
    layout.painter.nine_slice(
        icons::PANEL,
        field.x,
        field.y,
        field.w,
        field.h,
        icons::PANEL_INSET,
        colors::SEARCH_FIELD,
    );
    layout.painter.nine_slice(
        icons::PANEL_BORDER,
        field.x,
        field.y,
        field.w,
        field.h,
        icons::PANEL_INSET,
        border,
    );

    // Крестик стирает набранное; появляется, только когда есть что стирать.
    // Размер у него свой, натуральный: игра кладёт `SearchCancel` как есть,
    // прижав к правому краю с отступом в два пикселя.
    let mut text_w = field.w - pad * 2.0;
    if !ui.search.is_empty() {
        let side = (SEARCH_CANCEL_SIZE * ui_scale).round();
        let inset = (2.0 * ui_scale).round();
        let cancel = Rect {
            x: field.x + field.w - side - inset,
            y: (field.y + (field.h - side) * 0.5).round(),
            w: side,
            h: side,
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
        text_w -= side + inset;
    }

    // Текст в поле игра пишет мельче остального интерфейса: `UISearchBar`
    // заводится с `scale = 0.8f`. Пустую подсказку она красит в `Color.Gray`,
    // набранное — в белый.
    let text_scale = (ui_scale * SEARCH_TEXT).max(0.4);
    let outer_scale = layout.painter.scale;
    layout.painter.scale = text_scale;

    let text_x = field.x + pad * 2.0;
    let mut pen = text_x;
    if ui.search.is_empty() {
        layout
            .painter
            .text_left(pen, field.y, field.h, lang::t().search_hint, colors::HINT);
        pen += layout.painter.measure(lang::t().search_hint);
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
            (pen + 2.0 * text_scale).round(),
            (field.y + (field.h - line) * 0.5).round(),
            (2.0 * text_scale).round().max(1.0),
            line,
            colors::TEXT,
        );
    }
    layout.painter.scale = outer_scale;
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
    let t = lang::t();
    let rows: Vec<(&str, String)> = state::with(|s| {
        let secs = s.stats.seconds;
        vec![
            (
                t.stat_time,
                format!(
                    "{:02}:{:02}:{:02}",
                    secs / 3600,
                    (secs % 3600) / 60,
                    secs % 60
                ),
            ),
            (t.stat_caught, s.stats.caught.to_string()),
            (t.stat_crates, s.stats.crates.to_string()),
            (t.stat_skipped, s.stats.skipped.to_string()),
            (
                t.stat_bite,
                lang::fill(t.stat_seconds, &[&format!("{:.1}", s.stats.average_bite)]),
            ),
            (t.stat_potions, s.stats.potions.to_string()),
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
        .text(inner_x, cursor, t.tab_stats, colors::TITLE);
    cursor += row_h;

    for (label, value) in rows {
        let r = Rect {
            x: inner_x,
            y: cursor,
            w: inner_w,
            h: row_h,
        };
        layout.value_row(r, label, &value, colors::VALUE);
        cursor += row_h + row_gap;
    }
}
