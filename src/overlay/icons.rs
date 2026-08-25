//! Атлас иконок предметов из `Content/Images/Item_<id>.xnb`.
//!
//! Иконки лежат отдельными файлами в формате Color (проверено: у всех
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

/// Собирает атлас из иконок перечисленных предметов.
/// Отсутствующие или слишком большие пропускаются молча.
pub fn build(content: &Path, items: &[i32]) -> Option<IconAtlas> {
    let mut loaded: Vec<(i32, xnb::Image)> = Vec::with_capacity(items.len());
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

    // Полочная укладка: иконки близки по размеру, сложнее не нужно.
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
