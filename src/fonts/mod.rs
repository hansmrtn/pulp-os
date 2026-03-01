// Build-time rasterised bitmap fonts for e-ink rendering.
// TTFs rasterised by build.rs via fontdue into 1-bit tables in flash.
// Zero heap, zero parsing at runtime. Three sizes: 0=Small, 1=Medium, 2=Large.

pub mod bitmap;

pub mod font_data {
    include!(concat!(env!("OUT_DIR"), "/font_data.rs"));
}

use crate::drivers::strip::StripBuffer;
use bitmap::BitmapFont;

// body font by index: 0 = Small, 1 = Medium, 2 = Large
pub fn body_font(idx: u8) -> &'static BitmapFont {
    match idx {
        1 => &font_data::REGULAR_BODY_MEDIUM,
        2 => &font_data::REGULAR_BODY_LARGE,
        _ => &font_data::REGULAR_BODY_SMALL,
    }
}

// chrome font (button labels, quick-menu items, loading text, etc.);
// always returns the small body font regardless of the size setting
pub fn chrome_font() -> &'static BitmapFont {
    body_font(0)
}

pub fn heading_font(idx: u8) -> &'static BitmapFont {
    match idx {
        1 => &font_data::REGULAR_HEADING_MEDIUM,
        2 => &font_data::REGULAR_HEADING_LARGE,
        _ => &font_data::REGULAR_HEADING_SMALL,
    }
}

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
    fn from_fonts(
        regular: &'static BitmapFont,
        bold_candidate: &'static BitmapFont,
        italic_candidate: &'static BitmapFont,
        heading: &'static BitmapFont,
    ) -> Self {
        let bold = if bold_candidate.glyph('A').advance > 0 {
            bold_candidate
        } else {
            regular
        };
        let italic = if italic_candidate.glyph('A').advance > 0 {
            italic_candidate
        } else {
            regular
        };
        Self {
            regular,
            bold,
            italic,
            heading,
        }
    }

    pub fn for_size(idx: u8) -> Self {
        match idx {
            1 => Self::from_fonts(
                &font_data::REGULAR_BODY_MEDIUM,
                &font_data::BOLD_BODY_MEDIUM,
                &font_data::ITALIC_BODY_MEDIUM,
                &font_data::REGULAR_HEADING_MEDIUM,
            ),
            2 => Self::from_fonts(
                &font_data::REGULAR_BODY_LARGE,
                &font_data::BOLD_BODY_LARGE,
                &font_data::ITALIC_BODY_LARGE,
                &font_data::REGULAR_HEADING_LARGE,
            ),
            _ => Self::from_fonts(
                &font_data::REGULAR_BODY_SMALL,
                &font_data::BOLD_BODY_SMALL,
                &font_data::ITALIC_BODY_SMALL,
                &font_data::REGULAR_HEADING_SMALL,
            ),
        }
    }

    pub fn new() -> Self {
        Self::for_size(0)
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
