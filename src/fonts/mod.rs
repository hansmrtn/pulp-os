// Build-time rasterised bitmap fonts for e-ink text rendering
//
// TTF files in src/fonts/ are rasterised by build.rs using fontdue
// (on the host). The output is compact 1-bit bitmap tables in flash.
// At runtime: zero heap, zero parsing, just table lookups and blits.
// FontSet is Copy (four &'static pointers, 32 bytes).
//
// Three body sizes are available (indexed 0–2):
//   0 = Small  (body 14 px, heading 20 px)
//   1 = Medium (body 21 px, heading 28 px)
//   2 = Large  (body 30 px, heading 40 px)
// Use `FontSet::for_size(idx)` to obtain a sized set.

pub mod bitmap;

pub mod font_data {
    include!(concat!(env!("OUT_DIR"), "/font_data.rs"));
}

use crate::drivers::strip::StripBuffer;
use bitmap::BitmapFont;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    Regular,
    Bold,
    Italic,
    Heading,
}

#[derive(Clone, Copy)]
pub struct FontSet {
    regular: &'static BitmapFont,
    bold: &'static BitmapFont,
    italic: &'static BitmapFont,
    heading: &'static BitmapFont,
}

impl Default for FontSet {
    fn default() -> Self {
        Self::new()
    }
}

impl FontSet {
    pub fn small() -> Self {
        let regular = &font_data::REGULAR_BODY_SMALL;

        let bold = if font_data::BOLD_BODY_SMALL.glyph('A').advance > 0 {
            &font_data::BOLD_BODY_SMALL
        } else {
            regular
        };

        let italic = if font_data::ITALIC_BODY_SMALL.glyph('A').advance > 0 {
            &font_data::ITALIC_BODY_SMALL
        } else {
            regular
        };

        Self {
            regular,
            bold,
            italic,
            heading: &font_data::REGULAR_HEADING_SMALL,
        }
    }

    pub fn medium() -> Self {
        let regular = &font_data::REGULAR_BODY_MEDIUM;

        let bold = if font_data::BOLD_BODY_MEDIUM.glyph('A').advance > 0 {
            &font_data::BOLD_BODY_MEDIUM
        } else {
            regular
        };

        let italic = if font_data::ITALIC_BODY_MEDIUM.glyph('A').advance > 0 {
            &font_data::ITALIC_BODY_MEDIUM
        } else {
            regular
        };

        Self {
            regular,
            bold,
            italic,
            heading: &font_data::REGULAR_HEADING_MEDIUM,
        }
    }

    pub fn large() -> Self {
        let regular = &font_data::REGULAR_BODY_LARGE;

        let bold = if font_data::BOLD_BODY_LARGE.glyph('A').advance > 0 {
            &font_data::BOLD_BODY_LARGE
        } else {
            regular
        };

        let italic = if font_data::ITALIC_BODY_LARGE.glyph('A').advance > 0 {
            &font_data::ITALIC_BODY_LARGE
        } else {
            regular
        };

        Self {
            regular,
            bold,
            italic,
            heading: &font_data::REGULAR_HEADING_LARGE,
        }
    }

    pub fn for_size(idx: u8) -> Self {
        match idx {
            1 => Self::medium(),
            2 => Self::large(),
            _ => Self::small(),
        }
    }

    pub fn new() -> Self {
        Self::small()
    }

    #[inline]
    fn font(&self, style: Style) -> &'static BitmapFont {
        match style {
            Style::Regular => self.regular,
            Style::Bold => self.bold,
            Style::Italic => self.italic,
            Style::Heading => self.heading,
        }
    }

    #[inline]
    pub fn line_height(&self, style: Style) -> u16 {
        self.font(style).line_height
    }

    #[inline]
    pub fn ascent(&self, style: Style) -> u16 {
        self.font(style).ascent
    }

    #[inline]
    pub fn advance(&self, ch: char, style: Style) -> u8 {
        self.font(style).advance(ch)
    }

    #[inline]
    pub fn advance_byte(&self, b: u8, style: Style) -> u8 {
        let ch = if (0x20..=0x7E).contains(&b) {
            b as char
        } else {
            '?'
        };
        self.font(style).advance(ch)
    }

    #[inline]
    pub fn draw_char(
        &self,
        strip: &mut StripBuffer,
        ch: char,
        style: Style,
        cx: i32,
        baseline: i32,
    ) -> u8 {
        self.font(style).draw_char(strip, ch, cx, baseline)
    }

    pub fn draw_bytes(
        &self,
        strip: &mut StripBuffer,
        text: &[u8],
        style: Style,
        cx: i32,
        baseline: i32,
    ) -> i32 {
        self.font(style).draw_bytes(strip, text, cx, baseline)
    }

    pub fn draw_str(
        &self,
        strip: &mut StripBuffer,
        text: &str,
        style: Style,
        cx: i32,
        baseline: i32,
    ) -> i32 {
        self.font(style).draw_str(strip, text, cx, baseline)
    }
}
