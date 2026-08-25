//! Родной шрифт Terraria из `Content/Fonts/*.xnb`.
//!
//! Формат — не стоковый XNA `SpriteFont`, а собственный
//! `ReLogic.Graphics.DynamicSpriteFontReader` (сверено по декомпиляции
//! ReLogic.dll, извлечённой из ресурсов Terraria.exe):
//!
//! ```text
//! float spacing, int lineSpacing, char defaultCharacter, int pageCount
//! на каждую страницу: Texture2D, List<Rectangle> glyphs, List<Rectangle> padding,
//!                     List<char> characters, List<Vector3> kerning
//! ```
//!
//! Страниц у Mouse_Text полторы сотни (весь Юникод игры), текстуры в DXT3,
//! поэтому распаковываем только те страницы, где есть нужные нам символы,
//! и переупаковываем их глифы в один небольшой атлас.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use lzxd::{Lzxd, WindowSize};

/// Ширина итогового атласа при переупаковке.
const ATLAS_WIDTH: u32 = 512;
const GLYPH_GAP: u32 = 1;

#[derive(Clone, Copy)]
pub struct Glyph {
    /// Прямоугольник в переупакованном атласе.
    pub sx: u32,
    pub sy: u32,
    pub w: u32,
    pub h: u32,
    /// Смещение при отрисовке относительно курсора.
    pub off_x: f32,
    pub off_y: f32,
    /// На сколько сдвинуть курсор после символа.
    pub advance: f32,
}

pub struct GameFont {
    pub width: u32,
    pub height: u32,
    /// A8R8G8B8 — как ждёт D3DFMT_A8R8G8B8.
    pub pixels: Vec<u32>,
    pub line_height: f32,
    pub space_advance: f32,
    pub glyphs: HashMap<char, Glyph>,
}

// ---------------------------------------------------------------------------
// Чтение потока
// ---------------------------------------------------------------------------

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    /// Целое в 7-битной кодировке, как его пишет BinaryWriter.
    fn varint(&mut self) -> Option<u32> {
        let mut result = 0u32;
        let mut shift = 0;
        loop {
            let byte = self.u8()?;
            result |= ((byte & 0x7F) as u32) << shift;
            if byte & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
            if shift > 28 {
                return None;
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        let len = self.varint()? as usize;
        Some(String::from_utf8(self.take(len)?.to_vec()).ok()?)
    }

    /// `BinaryReader.ReadChar` читает символ в UTF-8, 1..4 байта.
    fn char(&mut self) -> Option<char> {
        let first = self.u8()?;
        let extra = match first {
            0x00..=0x7F => 0,
            0xC0..=0xDF => 1,
            0xE0..=0xEF => 2,
            _ => 3,
        };
        let mut buffer = vec![first];
        for _ in 0..extra {
            buffer.push(self.u8()?);
        }
        std::str::from_utf8(&buffer).ok()?.chars().next()
    }

    fn rect(&mut self) -> Option<[i32; 4]> {
        Some([self.i32()?, self.i32()?, self.i32()?, self.i32()?])
    }

    fn list<T>(&mut self, mut item: impl FnMut(&mut Reader<'a>) -> Option<T>) -> Option<Vec<T>> {
        let _type_id = self.varint()?;
        let count = self.u32()? as usize;
        let mut out = Vec::with_capacity(count.min(8192));
        for _ in 0..count {
            out.push(item(self)?);
        }
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// Разбор
// ---------------------------------------------------------------------------

struct Page {
    format: i32,
    width: u32,
    height: u32,
    data: Range<usize>,
    glyphs: Vec<[i32; 4]>,
    padding: Vec<[i32; 4]>,
    chars: Vec<char>,
    kerning: Vec<[f32; 3]>,
}

pub fn load(path: &Path, wanted: &[char]) -> Option<GameFont> {
    let raw = std::fs::read(path).ok()?;
    let body = decompress(&raw)?;
    build(&body, wanted)
}

/// Снимает заголовок XNB и распаковывает LZX-поток.
fn decompress(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() < 14 || &raw[0..3] != b"XNB" {
        return None;
    }
    let flags = raw[5];
    let total = u32::from_le_bytes(raw[6..10].try_into().ok()?) as usize;

    if flags & 0x80 == 0 {
        return Some(raw.get(10..total.min(raw.len()))?.to_vec());
    }

    let target = u32::from_le_bytes(raw[10..14].try_into().ok()?) as usize;
    let mut lzx = Lzxd::new(WindowSize::KB64);
    let mut out: Vec<u8> = Vec::with_capacity(target);

    let mut pos = 14usize;
    let end = total.min(raw.len());
    while pos + 1 < end && out.len() < target {
        // Либо два байта размера блока, либо маркер 0xFF с явным размером кадра.
        let mut hi = raw[pos] as usize;
        let mut lo = raw[pos + 1] as usize;
        pos += 2;

        let mut frame = 0x8000usize;
        if hi == 0xFF {
            frame = (lo << 8) | *raw.get(pos)? as usize;
            pos += 1;
            hi = *raw.get(pos)? as usize;
            lo = *raw.get(pos + 1)? as usize;
            pos += 2;
        }
        let block = (hi << 8) | lo;
        if block == 0 || frame == 0 {
            break;
        }
        let frame = frame.min(target - out.len());
        let chunk = raw.get(pos..pos + block)?;
        pos += block;
        out.extend_from_slice(lzx.decompress_next(chunk, frame).ok()?);
    }

    (out.len() >= target).then_some(out)
}

fn build(body: &[u8], wanted: &[char]) -> Option<GameFont> {
    let mut r = Reader::new(body);

    let readers = r.varint()?;
    for _ in 0..readers {
        let _name = r.string()?;
        let _version = r.i32()?;
    }
    let _shared = r.varint()?;
    let _root = r.varint()?;

    let spacing = r.f32()?;
    let line_spacing = r.i32()?;
    let _default_char = r.char()?;
    let page_count = r.i32()?;
    if !(0..4096).contains(&page_count) {
        return None;
    }

    let mut pages = Vec::with_capacity(page_count as usize);
    for _ in 0..page_count {
        let _texture_type = r.varint()?;
        let format = r.i32()?;
        let width = r.u32()?;
        let height = r.u32()?;
        let mip_count = r.u32()?;
        if mip_count == 0 || width == 0 || height == 0 {
            return None;
        }
        let size = r.u32()? as usize;
        let start = r.pos;
        r.take(size)?;
        let data = start..start + size;
        for _ in 1..mip_count {
            let skip = r.u32()? as usize;
            r.take(skip)?;
        }

        let glyphs = r.list(|r| r.rect())?;
        let padding = r.list(|r| r.rect())?;
        let chars = r.list(|r| r.char())?;
        let kerning = r.list(|r| Some([r.f32()?, r.f32()?, r.f32()?]))?;

        pages.push(Page {
            format,
            width,
            height,
            data,
            glyphs,
            padding,
            chars,
            kerning,
        });
    }

    pack(body, &pages, wanted, spacing, line_spacing as f32)
}

/// Переупаковывает нужные глифы из страниц в один атлас.
fn pack(
    body: &[u8],
    pages: &[Page],
    wanted: &[char],
    spacing: f32,
    line_height: f32,
) -> Option<GameFont> {
    // Где искать каждый символ.
    let mut located: Vec<(char, usize, usize)> = Vec::new();
    for (page_index, page) in pages.iter().enumerate() {
        for (i, ch) in page.chars.iter().enumerate() {
            if wanted.contains(ch)
                && i < page.glyphs.len()
                && i < page.padding.len()
                && i < page.kerning.len()
            {
                located.push((*ch, page_index, i));
            }
        }
    }
    if located.is_empty() {
        return None;
    }

    // Полочная упаковка: символы одного шрифта близки по высоте,
    // поэтому простой раскладки достаточно.
    let mut glyphs = HashMap::with_capacity(located.len());
    let mut pen_x = GLYPH_GAP;
    let mut pen_y = GLYPH_GAP;
    let mut shelf_h = 0u32;
    let mut placements: Vec<(char, usize, usize, u32, u32)> = Vec::new();

    for (ch, page_index, i) in &located {
        let rect = pages[*page_index].glyphs[*i];
        let (w, h) = (rect[2].max(0) as u32, rect[3].max(0) as u32);
        if pen_x + w + GLYPH_GAP > ATLAS_WIDTH {
            pen_x = GLYPH_GAP;
            pen_y += shelf_h + GLYPH_GAP;
            shelf_h = 0;
        }
        placements.push((*ch, *page_index, *i, pen_x, pen_y));
        pen_x += w + GLYPH_GAP;
        shelf_h = shelf_h.max(h);
    }
    let atlas_h = (pen_y + shelf_h + GLYPH_GAP).max(1);
    let mut pixels = vec![0u32; (ATLAS_WIDTH * atlas_h) as usize];

    // Пиксели страниц распаковываем лениво и только те, что понадобились.
    let mut decoded: HashMap<usize, Vec<u32>> = HashMap::new();
    for (ch, page_index, i, x, y) in placements {
        let page = &pages[page_index];
        if !decoded.contains_key(&page_index) {
            let source = body.get(page.data.clone())?;
            decoded.insert(
                page_index,
                decode_surface(page.format, page.width, page.height, source)?,
            );
        }
        let surface = &decoded[&page_index];

        let rect = page.glyphs[i];
        let crop = page.padding[i];
        let kern = page.kerning[i];
        let (gx, gy) = (rect[0].max(0) as u32, rect[1].max(0) as u32);
        let (gw, gh) = (rect[2].max(0) as u32, rect[3].max(0) as u32);

        for row in 0..gh {
            let src_y = gy + row;
            if src_y >= page.height {
                break;
            }
            for col in 0..gw {
                let src_x = gx + col;
                if src_x >= page.width {
                    break;
                }
                let value = surface[(src_y * page.width + src_x) as usize];
                pixels[((y + row) * ATLAS_WIDTH + x + col) as usize] = value;
            }
        }

        glyphs.insert(
            ch,
            Glyph {
                sx: x,
                sy: y,
                w: gw,
                h: gh,
                off_x: kern[0] + crop[0] as f32,
                off_y: crop[1] as f32,
                advance: kern[0] + kern[1] + kern[2] + spacing,
            },
        );
    }

    let space_advance = glyphs
        .get(&' ')
        .map(|g| g.advance)
        .unwrap_or(line_height * 0.35);

    Some(GameFont {
        width: ATLAS_WIDTH,
        height: atlas_h,
        pixels,
        line_height,
        space_advance,
        glyphs,
    })
}

// ---------------------------------------------------------------------------
// Поверхности
// ---------------------------------------------------------------------------

fn decode_surface(format: i32, width: u32, height: u32, data: &[u8]) -> Option<Vec<u32>> {
    match format {
        0 => decode_color(width, height, data),
        5 => decode_dxt3(width, height, data),
        _ => None,
    }
}

fn decode_color(width: u32, height: u32, data: &[u8]) -> Option<Vec<u32>> {
    let count = (width * height) as usize;
    if data.len() < count * 4 {
        return None;
    }
    Some(
        data.chunks_exact(4)
            .take(count)
            .map(|p| {
                let (r, g, b, a) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
                (a << 24) | (r << 16) | (g << 8) | b
            })
            .collect(),
    )
}

/// DXT3 (BC2): 8 байт альфы по 4 бита на пиксель + цветовой блок DXT1.
fn decode_dxt3(width: u32, height: u32, data: &[u8]) -> Option<Vec<u32>> {
    let blocks_x = width.div_ceil(4);
    let blocks_y = height.div_ceil(4);
    if data.len() < (blocks_x * blocks_y * 16) as usize {
        return None;
    }
    let mut out = vec![0u32; (width * height) as usize];

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let offset = ((by * blocks_x + bx) * 16) as usize;
            let block = &data[offset..offset + 16];

            let alpha = u64::from_le_bytes(block[0..8].try_into().ok()?);
            let c0 = u16::from_le_bytes(block[8..10].try_into().ok()?);
            let c1 = u16::from_le_bytes(block[10..12].try_into().ok()?);
            let indices = u32::from_le_bytes(block[12..16].try_into().ok()?);

            let (r0, g0, b0) = rgb565(c0);
            let (r1, g1, b1) = rgb565(c1);
            // В BC2 цветовой блок всегда четырёхцветный.
            let palette = [
                (r0, g0, b0),
                (r1, g1, b1),
                (
                    ((2 * r0 as u32 + r1 as u32) / 3) as u8,
                    ((2 * g0 as u32 + g1 as u32) / 3) as u8,
                    ((2 * b0 as u32 + b1 as u32) / 3) as u8,
                ),
                (
                    ((r0 as u32 + 2 * r1 as u32) / 3) as u8,
                    ((g0 as u32 + 2 * g1 as u32) / 3) as u8,
                    ((b0 as u32 + 2 * b1 as u32) / 3) as u8,
                ),
            ];

            for row in 0..4u32 {
                for col in 0..4u32 {
                    let x = bx * 4 + col;
                    let y = by * 4 + row;
                    if x >= width || y >= height {
                        continue;
                    }
                    let texel = row * 4 + col;
                    let index = ((indices >> (texel * 2)) & 0x3) as usize;
                    let a4 = ((alpha >> (texel * 4)) & 0xF) as u32;
                    let a = (a4 * 255 / 15) as u32;
                    let (r, g, b) = palette[index];
                    out[(y * width + x) as usize] =
                        (a << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
                }
            }
        }
    }
    Some(out)
}

fn rgb565(value: u16) -> (u8, u8, u8) {
    let r = ((value >> 11) & 0x1F) as u32;
    let g = ((value >> 5) & 0x3F) as u32;
    let b = (value & 0x1F) as u32;
    (
        ((r * 255 + 15) / 31) as u8,
        ((g * 255 + 31) / 63) as u8,
        ((b * 255 + 15) / 31) as u8,
    )
}
