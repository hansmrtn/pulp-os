// pre-rasterised 1-bit bitmap font types
// data in flash via &'static refs from build.rs; packed MSB-first, row-major
//
// Two glyph tables per font:
//   • ASCII 0x20–0x7E: contiguous direct-indexed (fast, zero-search)
//   • Extended Unicode: sorted codepoint array, binary-searched at runtime
//
// Characters not found in either table render as '?' (ASCII fallback).

use embedded_graphics_core::geometry::Size;
use embedded_graphics_core::pixelcolor::BinaryColor;

use crate::drivers::strip::StripBuffer;
use crate::ui::{Alignment, Region};

// ── ASCII range ─────────────────────────────────────────────────────

pub const FIRST_CHAR: u8 = 0x20;
pub const LAST_CHAR: u8 = 0x7E;
pub const GLYPH_COUNT: usize = (LAST_CHAR - FIRST_CHAR + 1) as usize;

/// Map a raw byte to a printable ASCII `char`.
///
/// Bytes outside the printable ASCII range are replaced with `'?'`.
#[inline]
pub fn byte_to_char(b: u8) -> char {
    if (FIRST_CHAR..=LAST_CHAR).contains(&b) {
        b as char
    } else {
        '?'
    }
}

// ── glyph data ──────────────────────────────────────────────────────

/// Metrics and bitmap location for a single rasterised glyph.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct BitmapGlyph {
    /// Horizontal advance width in pixels.
    pub advance: u8,
    /// Horizontal offset from the cursor to the glyph's left edge.
    pub offset_x: i8,
    /// Vertical offset from the baseline to the glyph's top edge
    /// (negative = above baseline).
    pub offset_y: i8,
    /// Glyph bitmap width in pixels.
    pub width: u8,
    /// Glyph bitmap height in pixels.
    pub height: u8,
    /// Byte offset into the corresponding bitmap array.
    pub bitmap_offset: u16,
}

// ── BitmapFont ──────────────────────────────────────────────────────

/// A pre-rasterised 1-bit bitmap font stored in flash.
///
/// Contains two glyph sets:
/// - **ASCII** (`glyphs` / `bitmaps`): direct-indexed for U+0020–U+007E.
/// - **Extended** (`ext_codepoints` / `ext_glyphs` / `ext_bitmaps`):
///   sorted by codepoint, binary-searched at runtime.
///
/// Generated at build time by `build.rs`; zero heap, zero parsing.
pub struct BitmapFont {
    /// ASCII glyphs, indexed by `(char as u8) - FIRST_CHAR`.
    pub glyphs: &'static [BitmapGlyph; GLYPH_COUNT],
    /// Packed 1-bit bitmap data for ASCII glyphs.
    pub bitmaps: &'static [u8],

    /// Sorted array of extended Unicode codepoints.
    pub ext_codepoints: &'static [u32],
    /// Extended glyph metrics, parallel to `ext_codepoints`.
    pub ext_glyphs: &'static [BitmapGlyph],
    /// Packed 1-bit bitmap data for extended glyphs.
    pub ext_bitmaps: &'static [u8],

    /// Line height in pixels (ascent + descent + leading).
    pub line_height: u16,
    /// Ascent in pixels (baseline to top of tallest glyph).
    pub ascent: u16,
}

/// Result of a glyph lookup: the glyph metrics and which bitmap
/// table to use for rendering.
#[derive(Clone, Copy)]
pub struct ResolvedGlyph<'a> {
    pub glyph: &'a BitmapGlyph,
    pub bitmaps: &'a [u8],
}

impl BitmapFont {
    // ── glyph lookup ────────────────────────────────────────────────

    /// Look up a character and return its glyph metrics.
    ///
    /// For ASCII (U+0020–U+007E) this is a direct array index.
    /// For extended Unicode, a binary search over the sorted codepoint
    /// table.  Characters not in either table fall back to the space
    /// glyph (index 0).
    #[inline]
    pub fn glyph(&self, ch: char) -> &BitmapGlyph {
        self.resolve(ch).glyph
    }

    /// Look up a character and return both the glyph and the correct
    /// bitmap slice for rendering.
    pub fn resolve(&self, ch: char) -> ResolvedGlyph<'_> {
        let code = ch as u32;

        // fast path: ASCII
        if code >= FIRST_CHAR as u32 && code <= LAST_CHAR as u32 {
            return ResolvedGlyph {
                glyph: &self.glyphs[(code - FIRST_CHAR as u32) as usize],
                bitmaps: self.bitmaps,
            };
        }

        // extended Unicode: binary search
        if let Ok(idx) = self.ext_codepoints.binary_search(&code) {
            return ResolvedGlyph {
                glyph: &self.ext_glyphs[idx],
                bitmaps: self.ext_bitmaps,
            };
        }

        // fallback: '?' glyph from ASCII table
        let q_idx = (b'?' - FIRST_CHAR) as usize;
        ResolvedGlyph {
            glyph: &self.glyphs[q_idx],
            bitmaps: self.bitmaps,
        }
    }

    /// Returns `true` if this font has a glyph for `ch` (not just
    /// the fallback '?').
    #[inline]
    pub fn has_glyph(&self, ch: char) -> bool {
        let code = ch as u32;
        if code >= FIRST_CHAR as u32 && code <= LAST_CHAR as u32 {
            return true;
        }
        self.ext_codepoints.binary_search(&code).is_ok()
    }

    // ── measurement ─────────────────────────────────────────────────

    /// Horizontal advance for a single character.
    #[inline]
    pub fn advance(&self, ch: char) -> u8 {
        self.glyph(ch).advance
    }

    /// Total width in pixels of a `&str`.
    #[inline]
    pub fn measure_str(&self, text: &str) -> u16 {
        text.chars().map(|c| self.advance(c) as u16).sum()
    }

    /// Total width in pixels of a `&[u8]` slice (decodes UTF-8).
    pub fn measure_bytes(&self, text: &[u8]) -> u16 {
        Utf8Iter::new(text).map(|c| self.advance(c) as u16).sum()
    }

    // ── drawing (single character) ──────────────────────────────────

    /// Draw a character at `(cx, baseline)` in black.  Returns advance.
    #[inline]
    pub fn draw_char(&self, strip: &mut StripBuffer, ch: char, cx: i32, baseline: i32) -> u8 {
        self.draw_char_fg(strip, ch, BinaryColor::On, cx, baseline)
    }

    /// Draw a character at `(cx, baseline)` with the given foreground
    /// colour.  Returns the horizontal advance.
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

    // ── drawing (strings) ───────────────────────────────────────────

    /// Draw a `&str` at `(cx, baseline)` in black.  Returns final X.
    pub fn draw_str(&self, strip: &mut StripBuffer, text: &str, cx: i32, baseline: i32) -> i32 {
        self.draw_str_fg(strip, text, BinaryColor::On, cx, baseline)
    }

    /// Draw a `&str` at `(cx, baseline)` with the given foreground.
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

    /// Draw a `&[u8]` slice (decoded as UTF-8) at `(cx, baseline)` in
    /// black.  Returns the final X cursor position.
    pub fn draw_bytes(&self, strip: &mut StripBuffer, text: &[u8], cx: i32, baseline: i32) -> i32 {
        let mut x = cx;
        for ch in Utf8Iter::new(text) {
            x += self.draw_char(strip, ch, x, baseline) as i32;
        }
        x
    }

    /// Draw a `&[u8]` slice (decoded as UTF-8) with the given
    /// foreground colour.  Returns the final X cursor position.
    pub fn draw_bytes_fg(
        &self,
        strip: &mut StripBuffer,
        text: &[u8],
        fg: BinaryColor,
        cx: i32,
        baseline: i32,
    ) -> i32 {
        let mut x = cx;
        for ch in Utf8Iter::new(text) {
            x += self.draw_char_fg(strip, ch, fg, x, baseline) as i32;
        }
        x
    }

    // ── aligned drawing ─────────────────────────────────────────────

    /// Draw a `&str` aligned within a [`Region`].
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

// ── glyph blitting ──────────────────────────────────────────────────

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

// ── minimal UTF-8 byte-slice iterator ───────────────────────────────
//
// Decodes a `&[u8]` slice one `char` at a time.  Invalid sequences
// are replaced with U+FFFD (which will render as '?' via the font
// fallback).  This avoids pulling in `core::str::from_utf8` for
// byte-oriented text buffers.

/// Byte-level UTF-8 decoder that yields `char` values.
pub struct Utf8Iter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Utf8Iter<'a> {
    /// Create a new iterator over the given byte slice.
    #[inline]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

impl Iterator for Utf8Iter<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        if self.pos >= self.data.len() {
            return None;
        }

        let b0 = self.data[self.pos];

        // single-byte ASCII
        if b0 < 0x80 {
            self.pos += 1;
            return Some(b0 as char);
        }

        // determine expected sequence length from lead byte
        let (mut cp, expected) = if b0 < 0xC0 {
            // stray continuation byte — skip it
            self.pos += 1;
            return Some('\u{FFFD}');
        } else if b0 < 0xE0 {
            ((b0 as u32) & 0x1F, 2)
        } else if b0 < 0xF0 {
            ((b0 as u32) & 0x0F, 3)
        } else if b0 < 0xF8 {
            ((b0 as u32) & 0x07, 4)
        } else {
            self.pos += 1;
            return Some('\u{FFFD}');
        };

        // check that we have enough bytes
        if self.pos + expected > self.data.len() {
            self.pos = self.data.len();
            return Some('\u{FFFD}');
        }

        // decode continuation bytes
        for i in 1..expected {
            let cont = self.data[self.pos + i];
            if cont & 0xC0 != 0x80 {
                // broken sequence — consume up to the bad byte
                self.pos += i;
                return Some('\u{FFFD}');
            }
            cp = (cp << 6) | (cont as u32 & 0x3F);
        }

        self.pos += expected;
        Some(char::from_u32(cp).unwrap_or('\u{FFFD}'))
    }
}
