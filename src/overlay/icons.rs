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
pub const BUTTON: i32 = -7;
pub const TOGGLE_OFF: i32 = -8;
pub const TOGGLE_ON: i32 = -9;
pub const BAR_TRACK: i32 = -10;
pub const BAR_HANDLE: i32 = -11;
// Цветные ячейки инвентаря: игра держит их отдельными текстурами и берёт
// нужную по смыслу слота. Тонировать обычную бесполезно — она тёмно-синяя,
// умножение цветом даёт грязь.
pub const SLOT_ALLOW: i32 = -12;
pub const SLOT_DENY: i32 = -13;
pub const SLOT_HOVER: i32 = -14;

/// Ширина рамки при девятичастной нарезке, в пикселях исходной текстуры.
/// Для панели это раскладка самой игры: 12 + 4 + 12 = 28.
pub const PANEL_INSET: f32 = 12.0;
pub const INNER_INSET: f32 = 8.0;
pub const BUTTON_INSET: f32 = 8.0;
pub const BAR_INSET: f32 = 6.0;

/// Что взять из `Content/Images`: id, путь без расширения, вырезка.
const UI_ASSETS: &[(i32, &str, Option<[u32; 4]>)] = &[
    (CURSOR, "UI/Cursor_0", None),
    (PANEL, "UI/PanelBackground", None),
    (PANEL_BORDER, "UI/PanelBorder", None),
    (INNER_PANEL, "UI/InnerPanelBackground", None),
    (SLOT, "Inventory_Back", None),
    (SLOT_ALLOW, "Inventory_Back3", None),
    (SLOT_DENY, "Inventory_Back5", None),
    (SLOT_HOVER, "Inventory_Back13", None),
    (BUTTON, "UI/ButtonBacking", None),
    // Переключатель из меню настроек: слева кольцо «выкл», справа диск «вкл».
    (TOGGLE_OFF, "UI/Settings_Toggle", Some([0, 0, 14, 14])),
    (TOGGLE_ON, "UI/Settings_Toggle", Some([16, 0, 14, 14])),
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
/// Отсутствующие или слишком большие иконки пропускаются молча.
pub fn build(content: &Path, items: &[i32]) -> Option<IconAtlas> {
    let mut loaded: Vec<(i32, xnb::Image)> = Vec::with_capacity(items.len() + UI_ASSETS.len() + 1);

    loaded.push((WHITE, white_block()));
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

    for &id in items {
        let path = content.join(format!("Item_{id}.xnb"));
        let Some(image) = xnb::load_texture(&path) else {
            continue;
        };
        if image.width == 0 || image.height == 0 || image.width > MAX_ICON * 4 {
            continue;
        }
        loaded.push((id, image));
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
