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
/// Красный крестик поверх отвергнутого предмета — тот самый, что игра
/// кладёт на неразблокированное в меню дублирования: `Content/Images/CoolDown`
/// (`TextureAssets.Cd`), см. `ItemSlot.Draw`, ветка контекста 34
/// `CreativeInfiniteLocked`.
pub const CROSS: i32 = -14;
pub const SEARCH: i32 = -15;
pub const SEARCH_CANCEL: i32 = -16;
/// Уголки на кнопке сворачивания. Стрелок в шрифте игры нет, а готовой
/// картинки в `UI/` не нашлось, поэтому рисуем сами — ступеньками по
/// пикселям, как рисует свою графику игра.
pub const CHEVRON_UP: i32 = -17;
pub const CHEVRON_DOWN: i32 = -18;
/// Золотая рамка наведения под кнопку поиска. Середина прозрачная,
/// так что кладётся поверх. Широкой рамки под строку у игры нет: фокус
/// поля она показывает не картинкой, а цветом обводки, см. `search_field`.
pub const FRAME_SMALL: i32 = -19;
/// Сундук со стрелками — кнопка «разложить по ближайшим сундукам» из
/// инвентаря игры. Ровно так игра и различает состояния: `ChestStack_0`
/// обычный, `ChestStack_1` подсвечен (`Main.DrawInventory`, `num80`).
pub const CHEST_OFF: i32 = -21;
pub const CHEST_ON: i32 = -22;
/// Режим списка в фильтре: катушка «Высокопрочной лески» (`Item_2373`).
/// Светлая — белый список, притемнённая — чёрный. Вторая делается из первой
/// в `darkened`, отдельной картинки в игре под неё нет.
pub const LIST_WHITE: i32 = -23;
pub const LIST_BLACK: i32 = -24;
/// Насколько притемняется катушка под чёрный список, по каналу.
const LIST_DARKEN: u8 = 80;
/// Строка про врагов: включено — иконка баффа «Ездовой единорог»
/// (`Buff_162`), выключено — «Ездовой величественный скакун» (`Buff_276`)
/// без цвета. Обесцвеченной картинки в игре нет, делаем из обычной.
pub const ENEMY_ON: i32 = -25;
pub const ENEMY_OFF_SOURCE: i32 = -26;
pub const ENEMY_OFF: i32 = -27;
/// Строка про автопитьё: чашка кофе (`Item_5042`), включено в цвете,
/// выключено обесцвеченной. Картинка — лента из трёх кадров по 32x26,
/// берём верхний.
pub const POTION_ON: i32 = -28;
pub const POTION_OFF: i32 = -29;

/// Ширина рамки при девятичастной нарезке, в пикселях исходной текстуры.
/// Для панели это раскладка самой игры: 12 + 4 + 12 = 28.
pub const PANEL_INSET: f32 = 12.0;
pub const BAR_INSET: f32 = 6.0;
/// Уголки золотых рамок: у обеих скругление в шесть пикселей.
pub const FRAME_INSET: f32 = 6.0;

/// Что взять из `Content/Images`: id, путь без расширения, вырезка.
const UI_ASSETS: &[(i32, &str, Option<[u32; 4]>)] = &[
    (CURSOR, "UI/Cursor_0", None),
    (PANEL, "UI/PanelBackground", None),
    (PANEL_BORDER, "UI/PanelBorder", None),
    (SLOT, "Inventory_Back", None),
    (SLOT_MARK, "Inventory_Back15", None),
    (SLOT_HOVER, "Inventory_Back13", None),
    (CROSS, "CoolDown", None),
    (SEARCH, "UI/Bestiary/Button_Search", None),
    (SEARCH_CANCEL, "UI/SearchCancel", None),
    (TOGGLE_OFF, "UI/Bestiary/Icon_Rank_Dim", None),
    (TOGGLE_ON, "UI/Bestiary/Icon_Rank_Light", None),
    (CHEST_OFF, "UI/ChestStack_0", None),
    (CHEST_ON, "UI/ChestStack_1", None),
    (LIST_WHITE, "Item_2373", None),
    (ENEMY_ON, "Buff_162", None),
    (ENEMY_OFF_SOURCE, "Buff_276", None),
    // У кофе в файле лента из трёх кадров сверху вниз, нужен верхний.
    (POTION_ON, "Item_5042", Some([0, 0, 32, 26])),
    // Золотая рамка из бестиария: ей игра показывает наведение на кнопку.
    (FRAME_SMALL, "UI/Bestiary/Button_Search_Border", None),
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
    // Чёрный список — та же катушка, только притемнённая. Умножением цветом
    // при отрисовке так не получится: у светлых пикселей и у тёмных разница
    // одна и та же, а множитель растянул бы её в разы.
    if let Some((_, light)) = loaded.iter().find(|(id, _)| *id == LIST_WHITE) {
        let dark = darkened(light, LIST_DARKEN);
        loaded.push((LIST_BLACK, dark));
    }
    // Выключенные состояния — те же картинки без цвета. Так игра и сама
    // отличает погасшее от живого, но готовых серых копий у неё нет.
    for (source, target) in [(ENEMY_OFF_SOURCE, ENEMY_OFF), (POTION_ON, POTION_OFF)] {
        if let Some((_, color)) = loaded.iter().find(|(id, _)| *id == source) {
            let gray = grayscale(color);
            loaded.push((target, gray));
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

/// Та же картинка темнее: из каждого канала вычитается одно и то же,
/// прозрачность не трогается. Вычитание, а не умножение: так светлые места
/// уходят в тень вместе с тёмными и рисунок остаётся читаемым.
fn darkened(image: &xnb::Image, by: u8) -> xnb::Image {
    let pixels = image
        .pixels
        .iter()
        .map(|&p| {
            let channel = |shift: u32| (((p >> shift) & 0xFF) as u8).saturating_sub(by) as u32;
            (p & 0xFF00_0000) | (channel(16) << 16) | (channel(8) << 8) | channel(0)
        })
        .collect();
    xnb::Image {
        width: image.width,
        height: image.height,
        pixels,
    }
}

/// Та же картинка без цвета. Веса каналов — обычные для яркости: глаз
/// видит зелёный сильнее красного, а синий слабее всех, и без весов
/// картинка выходит плоской.
fn grayscale(image: &xnb::Image) -> xnb::Image {
    let pixels = image
        .pixels
        .iter()
        .map(|&p| {
            let channel = |shift: u32| ((p >> shift) & 0xFF) as f32;
            let gray = (channel(16) * 0.299 + channel(8) * 0.587 + channel(0) * 0.114)
                .round()
                .clamp(0.0, 255.0) as u32;
            (p & 0xFF00_0000) | (gray << 16) | (gray << 8) | gray
        })
        .collect();
    xnb::Image {
        width: image.width,
        height: image.height,
        pixels,
    }
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
