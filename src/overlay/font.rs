//! Атлас шрифта, отрисованный через GDI.
//!
//! На этом этапе берём системный моноширинный шрифт: он даёт кириллицу
//! без разбора XNB и LZX. Родной шрифт Terraria (`Content/Fonts/Mouse_Text.xnb`)
//! подключим позже, когда появится доступ к текстурам игры.
//!
//! Размер ячейки не задаётся на глаз, а берётся из метрик шрифта: иначе
//! широкие глифы (`Ш`, `Щ`, `@`) вылезают в соседнюю ячейку.

use std::collections::HashMap;

use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DeleteDC, DeleteObject, FF_MODERN,
    FONT_CLIP_PRECISION, FONT_QUALITY, GetTextMetricsW, HBITMAP, HGDIOBJ, OUT_DEFAULT_PRECIS,
    SelectObject, SetBkMode, SetTextColor, TEXTMETRICW, TRANSPARENT, TextOutW,
};
use windows::core::PCWSTR;

const COLS: u32 = 16;
const FONT_HEIGHT: i32 = 16;
/// Запас вокруг глифа, чтобы соседние ячейки не смешивались при билинейной фильтрации.
const GUTTER: u32 = 2;

pub struct FontAtlas {
    pub width: u32,
    pub height: u32,
    pub cell_w: u32,
    pub cell_h: u32,
    /// Шаг курсора при выводе строки — уже без запаса ячейки.
    pub advance: u32,
    /// Пиксели A8R8G8B8: цвет белый, прозрачность взята из яркости глифа.
    pub pixels: Vec<u32>,
    index: HashMap<char, u32>,
}

impl FontAtlas {
    /// Левый верхний угол ячейки символа, если он поддерживается.
    pub fn cell(&self, ch: char) -> Option<(u32, u32)> {
        let i = *self.index.get(&ch)?;
        Some(((i % COLS) * self.cell_w, (i / COLS) * self.cell_h))
    }
}

/// Набор символов: латиница, знаки, кириллица.
fn charset() -> Vec<char> {
    let mut chars: Vec<char> = (32u32..127).filter_map(char::from_u32).collect();
    chars.extend((0x410u32..=0x44f).filter_map(char::from_u32));
    chars.push('Ё');
    chars.push('ё');
    chars.push('—');
    chars.push('…');
    chars
}

pub fn build() -> Option<FontAtlas> {
    let chars = charset();
    let rows = chars.len().div_ceil(COLS as usize) as u32;

    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }

        let face: Vec<u16> = "Consolas\0".encode_utf16().collect();
        let font = CreateFontW(
            -FONT_HEIGHT,
            0,
            0,
            0,
            700, // FW_BOLD
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            FONT_CLIP_PRECISION(0),
            FONT_QUALITY(0),
            (DEFAULT_PITCH.0 | FF_MODERN.0) as u32,
            PCWSTR(face.as_ptr()),
        );
        let previous_font = SelectObject(dc, HGDIOBJ(font.0));

        // Размер ячейки — из метрик выбранного шрифта.
        let mut metrics = TEXTMETRICW::default();
        if !GetTextMetricsW(dc, &mut metrics).as_bool() {
            SelectObject(dc, previous_font);
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = DeleteDC(dc);
            return None;
        }
        // Шаг курсора — средняя ширина (шрифт моноширинный, значит она же
        // и есть реальный advance). Ячейка шире, под самые широкие глифы.
        let advance = metrics.tmAveCharWidth.max(1) as u32;
        let cell_w = (metrics.tmMaxCharWidth.max(1) as u32).max(advance) + GUTTER;
        let cell_h = (metrics.tmHeight + metrics.tmExternalLeading).max(1) as u32 + GUTTER;

        let width = COLS * cell_w;
        let height = rows * cell_h;

        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // Отрицательная высота — растр сверху вниз, как нам удобнее.
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bitmap: HBITMAP =
            match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) => b,
                Err(_) => {
                    SelectObject(dc, previous_font);
                    let _ = DeleteObject(HGDIOBJ(font.0));
                    let _ = DeleteDC(dc);
                    return None;
                }
            };
        if bits.is_null() {
            SelectObject(dc, previous_font);
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(dc);
            return None;
        }
        let previous_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));

        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00FF_FFFF));

        let mut index = HashMap::new();
        for (i, ch) in chars.iter().enumerate() {
            let i = i as u32;
            let x = ((i % COLS) * cell_w) as i32;
            let y = ((i / COLS) * cell_h) as i32;
            let text: Vec<u16> = ch.encode_utf16(&mut [0u16; 2]).to_vec();
            let _ = TextOutW(dc, x, y, &text);
            index.insert(*ch, i);
        }

        // Забираем растр до освобождения GDI-объектов.
        let count = (width * height) as usize;
        let raw = std::slice::from_raw_parts(bits as *const u32, count);
        let pixels: Vec<u32> = raw
            .iter()
            .map(|bgr| {
                let r = (bgr >> 16) & 0xFF;
                let g = (bgr >> 8) & 0xFF;
                let b = bgr & 0xFF;
                let alpha = r.max(g).max(b);
                (alpha << 24) | 0x00FF_FFFF
            })
            .collect();

        SelectObject(dc, previous_bitmap);
        SelectObject(dc, previous_font);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);

        Some(FontAtlas {
            width,
            height,
            cell_w,
            cell_h,
            advance,
            pixels,
            index,
        })
    }
}
