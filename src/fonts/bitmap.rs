// pre-rasterised 1-bit bitmap font types
// data in flash via &'static refs from build.rs, packed MSB-first, row-major
//
// two glyph tables per font:
//   ascii 0x20-0x7E: contiguous direct-indexed (fast, zero-search)
//   extended unicode: sorted codepoint array, binary-searched at runtime
//
// characters not found in either table render as '?' (ascii fallback)

use embedded_graphics_core::geometry::Size;
use embedded_graphics_core::pixelcolor::BinaryColor;

use crate::drivers::strip::StripBuffer;
use crate::ui::{Alignment, Region};

// re-export UTF-8 iterator from kernel util for convenience
pub use pulp_kernel::util::Utf8Iter;

pub const FIRST_CHAR: u8 = 0x20;
pub const LAST_CHAR: u8 = 0x7E;
pub const GLYPH_COUNT: usize = (LAST_CHAR - FIRST_CHAR + 1) as usize;

// map a raw byte to a printable ascii char, or '?' if out of range
#[inline]
pub fn byte_to_char(b: u8) -> char {
    if (FIRST_CHAR..=LAST_CHAR).contains(&b) {
        b as char
    } else {
        '?'
    }
}

// metrics and bitmap location for a single rasterised glyph
#[derive(Clone, Copy)]
#[repr(C)]
pub struct BitmapGlyph {
    pub advance: u8,        // horizontal advance width in pixels
    pub offset_x: i8,       // cursor to glyph left edge
    pub offset_y: i8,       // baseline to glyph top (negative = above)
    pub width: u8,          // bitmap width in pixels
    pub height: u8,         // bitmap height in pixels
    pub bitmap_offset: u16, // byte offset into bitmap array
}

// pre-rasterised 1-bit bitmap font stored in flash
//
// ascii glyphs are direct-indexed for 0x20-0x7E
// extended unicode glyphs are sorted by codepoint, binary-searched
// generated at build time by build.rs; zero heap, zero parsing
pub struct BitmapFont {
    pub glyphs: &'static [BitmapGlyph; GLYPH_COUNT], // ascii, indexed by (ch - FIRST_CHAR)
    pub bitmaps: &'static [u8],                      // packed 1-bit data for ascii

    pub ext_codepoints: &'static [u32], // sorted extended unicode codepoints
    pub ext_glyphs: &'static [BitmapGlyph], // parallel to ext_codepoints
    pub ext_bitmaps: &'static [u8],     // packed 1-bit data for extended

    pub line_height: u16, // ascent + descent + leading
    pub ascent: u16,      // baseline to top of tallest glyph
}

// result of a glyph lookup: metrics and which bitmap table to use
#[derive(Clone, Copy)]
pub struct ResolvedGlyph<'a> {
    pub glyph: &'a BitmapGlyph,
    pub bitmaps: &'a [u8],
}

impl BitmapFont {
    // look up a character, return glyph metrics
    // ascii: direct array index; extended: binary search
    // unknown chars fall back to '?' glyph
    #[inline]
    pub fn glyph(&self, ch: char) -> &BitmapGlyph {
        self.resolve(ch).glyph
    }

    // look up a character, return glyph + correct bitmap slice
    pub fn resolve(&self, ch: char) -> ResolvedGlyph<'_> {
        let code = ch as u32;

        // fast path: ascii
        if code >= FIRST_CHAR as u32 && code <= LAST_CHAR as u32 {
            return ResolvedGlyph {
                glyph: &self.glyphs[(code - FIRST_CHAR as u32) as usize],
                bitmaps: self.bitmaps,
            };
        }

        // extended unicode: binary search
        if let Ok(idx) = self.ext_codepoints.binary_search(&code) {
            return ResolvedGlyph {
                glyph: &self.ext_glyphs[idx],
                bitmaps: self.ext_bitmaps,
            };
        }

        // fallback: '?' from ascii table
        let q_idx = (b'?' - FIRST_CHAR) as usize;
        ResolvedGlyph {
            glyph: &self.glyphs[q_idx],
            bitmaps: self.bitmaps,
        }
    }

    // horizontal advance for a single character
    #[inline]
    pub fn advance(&self, ch: char) -> u8 {
        self.glyph(ch).advance
    }

    // total width in pixels of a &str
    #[inline]
    pub fn measure_str(&self, text: &str) -> u16 {
        text.chars().map(|c| self.advance(c) as u16).sum()
    }

    // total width in pixels of a &[u8] slice (decodes utf-8)
    pub fn measure_bytes(&self, text: &[u8]) -> u16 {
        Utf8Iter::new(text).map(|c| self.advance(c) as u16).sum()
    }

    // draw a character at (cx, baseline) in black, return advance
    #[inline]
    pub fn draw_char(&self, strip: &mut StripBuffer, ch: char, cx: i32, baseline: i32) -> u8 {
        self.draw_char_fg(strip, ch, BinaryColor::On, cx, baseline)
    }

    // draw a character with given foreground colour, return advance
    #[inline]
    pub fn draw_char_fg(
        &self,
        strip: &mut StripBuffer,
        ch: char,
        fg: BinaryColor,
        cx: i32,
        baseline: i32,
    ) -> u8 {
        let resolved = self.resolve(ch);
        let g = resolved.glyph;
        if g.width > 0 && g.height > 0 {
            blit_glyph(strip, resolved.bitmaps, g, fg, cx, baseline);
        }
        g.advance
    }

    // draw a &str at (cx, baseline) in black, return final x
    pub fn draw_str(&self, strip: &mut StripBuffer, text: &str, cx: i32, baseline: i32) -> i32 {
        self.draw_str_fg(strip, text, BinaryColor::On, cx, baseline)
    }

    // draw a &str with given foreground, return final x
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

    // draw a &str aligned within a region
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

// UTF-8 iteration is provided by pulp_kernel::util::Utf8Iter (re-exported above)
