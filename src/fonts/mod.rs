// build-time rasterised bitmap fonts for e-ink rendering
// TTFs rasterised by build.rs via fontdue into 1-bit tables in flash
// zero heap, zero parsing at runtime
//
// five size tiers: 0=XSmall  1=Small  2=Medium  3=Large  4=XLarge

pub mod bitmap;

#[allow(clippy::all)]
pub mod font_data {
    include!(concat!(env!("OUT_DIR"), "/font_data.rs"));
}

use crate::drivers::strip::StripBuffer;
use bitmap::BitmapFont;

pub const FONT_SIZE_COUNT: usize = 5;

const BODY_FONTS: [&BitmapFont; FONT_SIZE_COUNT] = [
    &font_data::REGULAR_BODY_XSMALL,
    &font_data::REGULAR_BODY_SMALL,
    &font_data::REGULAR_BODY_MEDIUM,
    &font_data::REGULAR_BODY_LARGE,
    &font_data::REGULAR_BODY_XLARGE,
];

const HEADING_FONTS: [&BitmapFont; FONT_SIZE_COUNT] = [
    &font_data::REGULAR_HEADING_XSMALL,
    &font_data::REGULAR_HEADING_SMALL,
    &font_data::REGULAR_HEADING_MEDIUM,
    &font_data::REGULAR_HEADING_LARGE,
    &font_data::REGULAR_HEADING_XLARGE,
];

const BOLD_FONTS: [&BitmapFont; FONT_SIZE_COUNT] = [
    &font_data::BOLD_BODY_XSMALL,
    &font_data::BOLD_BODY_SMALL,
    &font_data::BOLD_BODY_MEDIUM,
    &font_data::BOLD_BODY_LARGE,
    &font_data::BOLD_BODY_XLARGE,
];

const ITALIC_FONTS: [&BitmapFont; FONT_SIZE_COUNT] = [
    &font_data::ITALIC_BODY_XSMALL,
    &font_data::ITALIC_BODY_SMALL,
    &font_data::ITALIC_BODY_MEDIUM,
    &font_data::ITALIC_BODY_LARGE,
    &font_data::ITALIC_BODY_XLARGE,
];

pub const FONT_SIZE_NAMES: &[&str] = &["XSmall", "Small", "Medium", "Large", "XLarge"];

// pre-resolved body + heading font pair for a given size index
#[derive(Clone, Copy)]
pub struct UiFonts {
    pub body: &'static BitmapFont,
    pub heading: &'static BitmapFont,
}

impl UiFonts {
    pub fn for_size(idx: u8) -> Self {
        Self {
            body: body_font(idx),
            heading: heading_font(idx),
        }
    }
}

// human-readable name for size index (clamped to valid range)
#[inline]
pub fn font_size_name(idx: u8) -> &'static str {
    FONT_SIZE_NAMES
        .get(idx as usize)
        .copied()
        .unwrap_or("Small")
}

#[inline]
pub const fn max_size_idx() -> u8 {
    (FONT_SIZE_COUNT - 1) as u8
}

fn font_by_idx(table: &[&'static BitmapFont; FONT_SIZE_COUNT], idx: u8) -> &'static BitmapFont {
    table[(idx as usize).min(FONT_SIZE_COUNT - 1)]
}

pub fn body_font(idx: u8) -> &'static BitmapFont {
    font_by_idx(&BODY_FONTS, idx)
}

// chrome font (button labels, quick-menu items, loading text)
// always the XSmall body font, compact for UI chrome
pub fn chrome_font() -> &'static BitmapFont {
    &font_data::REGULAR_BODY_XSMALL
}

pub fn heading_font(idx: u8) -> &'static BitmapFont {
    font_by_idx(&HEADING_FONTS, idx)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    Regular,
    Bold,
    Italic,
    Heading,
}

// complete set of four style variants at a single size tier
// missing weights fall back to regular automatically
#[derive(Clone, Copy)]
pub struct FontSet {
    regular: &'static BitmapFont,
    bold: &'static BitmapFont,
    italic: &'static BitmapFont,
    heading: &'static BitmapFont,
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
        let i = (idx as usize).min(FONT_SIZE_COUNT - 1);
        Self::from_fonts(
            BODY_FONTS[i],
            BOLD_FONTS[i],
            ITALIC_FONTS[i],
            HEADING_FONTS[i],
        )
    }

    #[inline]
    pub fn font(&self, style: Style) -> &'static BitmapFont {
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
