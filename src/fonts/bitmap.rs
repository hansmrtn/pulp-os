// Pre-rasterised 1-bit bitmap font types.
// Data in flash via &'static refs from build.rs. Packed MSB-first, row-major.

use embedded_graphics_core::geometry::Size;
use embedded_graphics_core::pixelcolor::BinaryColor;

use crate::drivers::strip::StripBuffer;
use crate::ui::{Alignment, Region};

pub const FIRST_CHAR: u8 = 0x20;
pub const LAST_CHAR: u8 = 0x7E;
pub const GLYPH_COUNT: usize = (LAST_CHAR - FIRST_CHAR + 1) as usize; // 95

// map arbitrary byte to printable char; out-of-range becomes '?'
#[inline]
pub fn byte_to_char(b: u8) -> char {
    if (FIRST_CHAR..=LAST_CHAR).contains(&b) {
        b as char
    } else {
        '?'
    }
}

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
        if (FIRST_CHAR as u32..=LAST_CHAR as u32).contains(&code) {
            &self.glyphs[(code - FIRST_CHAR as u32) as usize]
        } else {
            &self.glyphs[0] // space
        }
    }

    #[inline]
    pub fn advance(&self, ch: char) -> u8 {
        self.glyph(ch).advance
    }

    // sum of advance widths for every character in text
    #[inline]
    pub fn measure_str(&self, text: &str) -> u16 {
        text.chars().map(|c| self.advance(c) as u16).sum()
    }

    // sum of advance widths for every byte; out-of-range bytes count as '?'
    #[inline]
    pub fn measure_bytes(&self, text: &[u8]) -> u16 {
        text.iter()
            .map(|&b| self.advance(byte_to_char(b)) as u16)
            .sum()
    }

    // draw a single glyph in BinaryColor::On (black); return advance width
    #[inline]
    pub fn draw_char(&self, strip: &mut StripBuffer, ch: char, cx: i32, baseline: i32) -> u8 {
        self.draw_char_fg(strip, ch, BinaryColor::On, cx, baseline)
    }

    // draw a single glyph in the given foreground colour; return advance width
    #[inline]
    pub fn draw_char_fg(
        &self,
        strip: &mut StripBuffer,
        ch: char,
        fg: BinaryColor,
        cx: i32,
        baseline: i32,
    ) -> u8 {
        let g = self.glyph(ch);
        if g.width > 0 && g.height > 0 {
            blit_glyph(strip, self.bitmaps, g, fg, cx, baseline);
        }
        g.advance
    }

    // draw text in BinaryColor::On; return x after the last glyph
    pub fn draw_str(&self, strip: &mut StripBuffer, text: &str, cx: i32, baseline: i32) -> i32 {
        self.draw_str_fg(strip, text, BinaryColor::On, cx, baseline)
    }

    // draw text in the given foreground colour; return x after the last glyph
    pub fn draw_str_fg(
        &self,
        strip: &mut StripBuffer,
        text: &str,
        fg: BinaryColor,
        cx: i32,
        baseline: i32,
    ) -> i32 {
        let mut x = cx;
        for ch in text.chars() {
            x += self.draw_char_fg(strip, ch, fg, x, baseline) as i32;
        }
        x
    }

    pub fn draw_bytes(&self, strip: &mut StripBuffer, text: &[u8], cx: i32, baseline: i32) -> i32 {
        let mut x = cx;
        for &b in text {
            x += self.draw_char(strip, byte_to_char(b), x, baseline) as i32;
        }
        x
    }

    // measure, align, and draw text; does not clear background
    pub fn draw_aligned(
        &self,
        strip: &mut StripBuffer,
        region: Region,
        text: &str,
        alignment: Alignment,
        fg: BinaryColor,
    ) {
        if text.is_empty() {
            return;
        }
        let text_w = self.measure_str(text) as u32;
        let text_h = self.line_height as u32;
        let top_left = alignment.position(region, Size::new(text_w, text_h));
        let baseline = top_left.y + self.ascent as i32;
        self.draw_str_fg(strip, text, fg, top_left.x, baseline);
    }
}

fn blit_glyph(
    strip: &mut StripBuffer,
    bitmaps: &[u8],
    g: &BitmapGlyph,
    fg: BinaryColor,
    cx: i32,
    baseline: i32,
) {
    let gx = cx + g.offset_x as i32;
    let gy = baseline + g.offset_y as i32;
    let w = g.width as usize;
    let h = g.height as usize;
    let stride = w.div_ceil(8);

    strip.blit_1bpp(
        bitmaps,
        g.bitmap_offset as usize,
        w,
        h,
        stride,
        gx,
        gy,
        fg == BinaryColor::On,
    );
}
