//! Общий атлас: иконки предметов из `Content/Images/Item_<id>.xnb`
//! и служебная графика интерфейса из `Content/Images/UI`.
//!
//! Картинки лежат отдельными файлами в формате Color (проверено: у всех
//! просмотренных `format = 0`), так что тот же XNB-ридер, что и для шрифта,
//! читает их без дополнительной работы. Нужные складываем в один атлас.

use std::collections::HashMap;
use std::path::Path;

use super::xnb;

/// Ширина атласа; высота считается по факту укладки.
const ATLAS_WIDTH: u32 = 1024;
const GAP: u32 = 2;
/// Иконки крупнее просто не влезут в ячейку сетки.
const MAX_ICON: u32 = 48;

// ---------------------------------------------------------------------------
// Служебная графика
// ---------------------------------------------------------------------------

// Id предметов у игры положительные, поэтому свои картинки кладём в тот же
// атлас под отрицательными — отдельная текстура и отдельная партия не нужны.

/// Белый квадрат: им закрашиваются обычные прямоугольники, чтобы вся
/// отрисовка шла одной партией и слои ложились в порядке вызовов.
pub const WHITE: i32 = -1;
pub const CURSOR: i32 = -2;
pub const PANEL: i32 = -3;
pub const PANEL_BORDER: i32 = -4;
pub const INNER_PANEL: i32 = -5;
pub const SLOT: i32 = -6;
/// Переключатели — звёздочки ранга из бестиария: тусклая и золотая.
pub const TOGGLE_OFF: i32 = -8;
pub const TOGGLE_ON: i32 = -9;
pub const BAR_TRACK: i32 = -10;
pub const BAR_HANDLE: i32 = -11;
// Ячейки инвентаря: игра держит варианты отдельными текстурами и берёт
// нужную по смыслу слота. Тонировать обычную бесполезно — она тёмно-синяя,
// умножение цветом даёт грязь, а вот у `Inventory_Back15` светлая рамка,
// и она принимает цвет: ровно так игра подсвечивает и новые предметы,
// и слоты, куда предмет положить можно или нельзя.
pub const SLOT_MARK: i32 = -12;
pub const SLOT_HOVER: i32 = -13;
/// Красный крестик поверх отвергнутого предмета. Своей такой картинки
/// у игры нет, поэтому рисуем её сами — зато одним квадом, а не лесенкой.
pub const CROSS: i32 = -14;
pub const SEARCH: i32 = -15;
pub const SEARCH_CANCEL: i32 = -16;
/// Уголки на кнопке сворачивания. Стрелок в шрифте игры нет, а готовой
/// картинки в `UI/` не нашлось, поэтому рисуем сами — ступеньками по
/// пикселям, как рисует свою графику игра.
pub const CHEVRON_UP: i32 = -17;
pub const CHEVRON_DOWN: i32 = -18;
/// Золотые рамки наведения и фокуса: квадратная под кнопку и широкая
/// под строку. Обе с прозрачной серединой, так что кладутся поверх.
pub const FRAME_SMALL: i32 = -19;
pub const FRAME_WIDE: i32 = -20;

/// Ширина рамки при девятичастной нарезке, в пикселях исходной текстуры.
/// Для панели это раскладка самой игры: 12 + 4 + 12 = 28.
pub const PANEL_INSET: f32 = 12.0;
pub const INNER_INSET: f32 = 8.0;
pub const BAR_INSET: f32 = 6.0;
/// Уголки золотых рамок: у обеих скругление в шесть пикселей.
pub const FRAME_INSET: f32 = 6.0;

/// Что взять из `Content/Images`: id, путь без расширения, вырезка.
const UI_ASSETS: &[(i32, &str, Option<[u32; 4]>)] = &[
    (CURSOR, "UI/Cursor_0", None),
    (PANEL, "UI/PanelBackground", None),
    (PANEL_BORDER, "UI/PanelBorder", None),
    (INNER_PANEL, "UI/InnerPanelBackground", None),
    (SLOT, "Inventory_Back", None),
    (SLOT_MARK, "Inventory_Back15", None),
    (SLOT_HOVER, "Inventory_Back13", None),
    (SEARCH, "UI/Bestiary/Button_Search", None),
    (SEARCH_CANCEL, "UI/SearchCancel", None),
    (TOGGLE_OFF, "UI/Bestiary/Icon_Rank_Dim", None),
    (TOGGLE_ON, "UI/Bestiary/Icon_Rank_Light", None),
    // Золотые рамки из бестиария: ими игра показывает наведение и фокус.
    (FRAME_SMALL, "UI/Bestiary/Button_Search_Border", None),
    (FRAME_WIDE, "UI/Bestiary/Button_Wide_Border", None),
    (BAR_TRACK, "UI/Scrollbar", None),
    (BAR_HANDLE, "UI/ScrollbarInner", None),
];

#[derive(Clone, Copy)]
pub struct IconRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

pub struct IconAtlas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
    map: HashMap<i32, IconRect>,
}

impl IconAtlas {
    pub fn get(&self, item: i32) -> Option<IconRect> {
        self.map.get(&item).copied()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// Собирает атлас: сначала служебная графика, потом иконки предметов.
/// У каждого предмета — число кадров анимации: у неподвижных единица,
/// у остальных в файле лежит лента кадров сверху вниз, и берём верхний.
/// Отсутствующие или слишком большие иконки пропускаются молча.
pub fn build(content: &Path, items: &[(i32, u32)]) -> Option<IconAtlas> {
    let mut loaded: Vec<(i32, xnb::Image)> = Vec::with_capacity(items.len() + UI_ASSETS.len() + 1);

    loaded.push((WHITE, white_block()));
    loaded.push((CROSS, cross_block()));
    loaded.push((CHEVRON_UP, chevron_block(true)));
    loaded.push((CHEVRON_DOWN, chevron_block(false)));
    for (id, name, cut) in UI_ASSETS {
        let path = content.join(format!("{name}.xnb"));
        let Some(image) = xnb::load_texture(&path) else {
            crate::log!("оверлей: {} не прочитан", path.display());
            continue;
        };
        match cut {
            Some(rect) => match crop(&image, *rect) {
                Some(part) => loaded.push((*id, part)),
                None => crate::log!("оверлей: вырезка {name} {rect:?} не влезла"),
            },
            None => loaded.push((*id, image)),
        }
    }

    for &(id, frames) in items {
        let path = content.join(format!("Item_{id}.xnb"));
        let Some(image) = xnb::load_texture(&path) else {
            continue;
        };
        if image.width == 0 || image.height == 0 || image.width > MAX_ICON * 4 {
            continue;
        }
        let frame = match frames {
            0 | 1 => Some(image),
            n => crop(&image, [0, 0, image.width, image.height / n]),
        };
        if let Some(frame) = frame {
            loaded.push((id, frame));
        }
    }
    if loaded.is_empty() {
        return None;
    }

    // Полочная укладка: картинки близки по размеру, сложнее не нужно.
    let mut map = HashMap::with_capacity(loaded.len());
    let mut placements: Vec<(usize, u32, u32)> = Vec::with_capacity(loaded.len());
    let mut pen_x = GAP;
    let mut pen_y = GAP;
    let mut shelf = 0u32;

    for (index, (_, image)) in loaded.iter().enumerate() {
        let (w, h) = (image.width, image.height);
        if pen_x + w + GAP > ATLAS_WIDTH {
            pen_x = GAP;
            pen_y += shelf + GAP;
            shelf = 0;
        }
        placements.push((index, pen_x, pen_y));
        pen_x += w + GAP;
        shelf = shelf.max(h);
    }
    let height = pen_y + shelf + GAP;
    let mut pixels = vec![0u32; (ATLAS_WIDTH * height) as usize];

    for (index, x, y) in placements {
        let (id, image) = &loaded[index];
        for row in 0..image.height {
            let dst = ((y + row) * ATLAS_WIDTH + x) as usize;
            let src = (row * image.width) as usize;
            pixels[dst..dst + image.width as usize]
                .copy_from_slice(&image.pixels[src..src + image.width as usize]);
        }
        map.insert(
            *id,
            IconRect {
                x,
                y,
                w: image.width,
                h: image.height,
            },
        );
    }

    Some(IconAtlas {
        width: ATLAS_WIDTH,
        height,
        pixels,
        map,
    })
}

/// Кусок картинки; за границы не выходим, иначе пропускаем.
fn crop(image: &xnb::Image, rect: [u32; 4]) -> Option<xnb::Image> {
    let [x, y, w, h] = rect;
    if x + w > image.width || y + h > image.height || w == 0 || h == 0 {
        return None;
    }
    let mut pixels = Vec::with_capacity((w * h) as usize);
    for row in 0..h {
        let start = ((y + row) * image.width + x) as usize;
        pixels.extend_from_slice(&image.pixels[start..start + w as usize]);
    }
    Some(xnb::Image {
        width: w,
        height: h,
        pixels,
    })
}

/// Непрозрачный белый квадратик под заливки. Четыре пикселя, а не один:
/// с запасом от соседей по атласу при любой фильтрации.
fn white_block() -> xnb::Image {
    xnb::Image {
        width: 4,
        height: 4,
        pixels: vec![0xFFFF_FFFF; 16],
    }
}

/// Уголок «раскрыть / свернуть»: ступеньки по два пикселя, как в графике
/// игры. Картинка квадратная и симметричная, поэтому центрируется просто
/// по своим границам, без подгонок на глаз.
fn chevron_block(up: bool) -> xnb::Image {
    const SIZE: u32 = 16;
    /// Толщина штриха и ширина ступеньки — в пикселях картинки.
    const THICK: u32 = 3;
    const STEP: u32 = 2;

    let mut pixels = vec![0u32; (SIZE * SIZE) as usize];
    let arms = SIZE / (STEP * 2);
    // Уголок ставим так, чтобы он занял середину квадрата по высоте.
    let base = (SIZE - arms * STEP - THICK) / 2;
    for arm in 0..arms {
        let row = base + arm * STEP;
        let row = if up { SIZE - row - THICK } else { row };
        let left = arm * STEP;
        let right = SIZE - (arm + 1) * STEP;
        for y in row..(row + THICK).min(SIZE) {
            for x in left..(left + STEP).min(SIZE) {
                pixels[(y * SIZE + x) as usize] = 0xFFFF_FFFF;
            }
            for x in right..(right + STEP).min(SIZE) {
                pixels[(y * SIZE + x) as usize] = 0xFFFF_FFFF;
            }
        }
    }
    // Смыкаем половинки на острие.
    let tip = base + arms * STEP;
    let tip = if up { SIZE - tip - THICK } else { tip };
    for y in tip..(tip + THICK).min(SIZE) {
        for x in (arms * STEP)..(SIZE - arms * STEP) {
            pixels[(y * SIZE + x) as usize] = 0xFFFF_FFFF;
        }
    }
    xnb::Image {
        width: SIZE,
        height: SIZE,
        pixels,
    }
}

/// Косой крест с тёмной обводкой — так нарисована вся мелкая графика игры:
/// светлое ядро в один-два пикселя и контур вокруг. Без контура крест
/// теряется на пёстрых иконках.
///
/// Ядро белое, цвет ему задаётся при отрисовке; обводка своя, чёрная.
fn cross_block() -> xnb::Image {
    const SIZE: u32 = 24;
    /// Полутолщина ядра и всего штриха вместе с обводкой, в пикселях.
    const CORE: f32 = 1.1;
    const EDGE: f32 = 2.3;

    let mut pixels = vec![0u32; (SIZE * SIZE) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Расстояние до ближней из двух диагоналей квадрата.
            let down = (fx - fy).abs();
            let up = (fx + fy - SIZE as f32).abs();
            let near = down.min(up) / std::f32::consts::SQRT_2;
            if near > EDGE {
                continue;
            }
            let pixel = if near <= CORE {
                0xFFFF_FFFF
            } else {
                // Обводка гаснет к краю: ступеньки на масштабе выглядят
                // грубее, чем мягкий край.
                let alpha = ((EDGE - near).min(1.0) * 255.0) as u32;
                alpha << 24
            };
            pixels[(y * SIZE + x) as usize] = pixel;
        }
    }
    xnb::Image {
        width: SIZE,
        height: SIZE,
        pixels,
    }
}
