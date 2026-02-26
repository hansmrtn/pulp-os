// Pre-rasterised 1-bit bitmap font types
//
// All data lives in flash (.rodata) via &'static references generated
// by build.rs. Zero heap at runtime. Glyph bitmaps are packed 1-bit,
// MSB-first, row-major, ceil(width/8) bytes per row.

use embedded_graphics_core::Pixel;
use embedded_graphics_core::pixelcolor::BinaryColor;
use embedded_graphics_core::prelude::*;

use crate::drivers::strip::StripBuffer;

pub const FIRST_CHAR: u8 = 0x20;
pub const LAST_CHAR: u8 = 0x7E;
pub const GLYPH_COUNT: usize = (LAST_CHAR - FIRST_CHAR + 1) as usize; // 95

#[derive(Clone, Copy)]
#[repr(C)]
pub struct BitmapGlyph {
    pub advance: u8,
    pub offset_x: i8,
    pub offset_y: i8,
    pub width: u8,
    pub height: u8,
    pub bitmap_offset: u16,
}

pub struct BitmapFont {
    pub glyphs: &'static [BitmapGlyph; GLYPH_COUNT],
    pub bitmaps: &'static [u8],
    pub line_height: u16,
    pub ascent: u16,
}

impl BitmapFont {
    #[inline]
    pub fn glyph(&self, ch: char) -> &BitmapGlyph {
        let code = ch as u32;
        if code >= FIRST_CHAR as u32 && code <= LAST_CHAR as u32 {
            &self.glyphs[(code - FIRST_CHAR as u32) as usize]
        } else {
            &self.glyphs[0] // space
        }
    }

    #[inline]
    pub fn advance(&self, ch: char) -> u8 {
        self.glyph(ch).advance
    }

    #[inline]
    pub fn draw_char(&self, strip: &mut StripBuffer, ch: char, cx: i32, baseline: i32) -> u8 {
        let g = self.glyph(ch);
        if g.width > 0 && g.height > 0 {
            blit_glyph(strip, self.bitmaps, g, cx, baseline);
        }
        g.advance
    }

    pub fn draw_str(&self, strip: &mut StripBuffer, text: &str, cx: i32, baseline: i32) -> i32 {
        let mut x = cx;
        for ch in text.chars() {
            x += self.draw_char(strip, ch, x, baseline) as i32;
        }
        x
    }

    pub fn draw_bytes(&self, strip: &mut StripBuffer, text: &[u8], cx: i32, baseline: i32) -> i32 {
        let mut x = cx;
        for &b in text {
            let ch = if b >= 0x20 && b <= 0x7E {
                b as char
            } else {
                '?'
            };
            x += self.draw_char(strip, ch, x, baseline) as i32;
        }
        x
    }
}

fn blit_glyph(strip: &mut StripBuffer, bitmaps: &[u8], g: &BitmapGlyph, cx: i32, baseline: i32) {
    let gx = cx + g.offset_x as i32;
    let gy = baseline + g.offset_y as i32;
    let w = g.width as usize;
    let h = g.height as usize;
    let stride = (w + 7) / 8;
    let base = g.bitmap_offset as usize;

    if base + stride * h > bitmaps.len() {
        return;
    }

    let pixels = (0..h).flat_map(move |y| {
        let row = base + y * stride;
        (0..w).filter_map(move |x| {
            if bitmaps[row + x / 8] & (1 << (7 - x % 8)) != 0 {
                Some(Pixel(
                    Point::new(gx + x as i32, gy + y as i32),
                    BinaryColor::On,
                ))
            } else {
                None
            }
        })
    });

    let _ = strip.draw_iter(pixels);
}
