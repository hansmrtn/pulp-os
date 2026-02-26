// Build-time rasterised bitmap fonts for e-ink text rendering
//
// TTF files in src/fonts/ are rasterised by build.rs using fontdue
// (on the host). The output is compact 1-bit bitmap tables in flash.
// At runtime: zero heap, zero parsing, just table lookups and blits.
// FontSet is Copy (four &'static pointers, 32 bytes).

pub mod bitmap;

pub mod font_data {
    include!(concat!(env!("OUT_DIR"), "/font_data.rs"));
}

use crate::board::strip::StripBuffer;
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

impl FontSet {
    pub fn new() -> Self {
        let regular = &font_data::REGULAR_BODY;

        let bold = if font_data::BOLD_BODY.glyph('A').advance > 0 {
            &font_data::BOLD_BODY
        } else {
            regular
        };

        let italic = if font_data::ITALIC_BODY.glyph('A').advance > 0 {
            &font_data::ITALIC_BODY
        } else {
            regular
        };

        Self {
            regular,
            bold,
            italic,
            heading: &font_data::REGULAR_HEADING,
        }
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
        let ch = if b >= 0x20 && b <= 0x7E {
            b as char
        } else {
            '?'
        };
        self.font(style).advance(ch)
    }

    pub fn measure(&self, text: &str, style: Style) -> u32 {
        self.font(style).measure(text)
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
