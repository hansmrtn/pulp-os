// Proportional-font button widget backed by the build-time bitmap pipeline.
//
// Mirrors the API of the mono-font Button widget (Outlined / Filled /
// Rounded styles, pressed/inverted state) but measures and renders text
// through BitmapFont instead of MonoFont.  Background fill and outline
// strokes are drawn via the DrawTarget impl on StripBuffer; glyph
// blitting uses BitmapFont::draw_str_fg so that pressed (inverted)
// buttons render white text on a black fill correctly.

use core::convert::Infallible;

use embedded_graphics::{
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{CornerRadii, PrimitiveStyle, RoundedRectangle},
};

use super::widget::{Alignment, Region};
use crate::drivers::strip::StripBuffer;
use crate::fonts::bitmap::BitmapFont;

// ── Style ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum BitmapButtonStyle {
    #[default]
    Outlined,
    Filled,
    Rounded(u32),
}

// ── BitmapButton ──────────────────────────────────────────────────────────────

pub struct BitmapButton<'a> {
    region: Region,
    label: &'a str,
    font: &'static BitmapFont,
    style: BitmapButtonStyle,
    pressed: bool,
}

impl<'a> BitmapButton<'a> {
    pub fn new(region: Region, label: &'a str, font: &'static BitmapFont) -> Self {
        Self {
            region,
            label,
            font,
            style: BitmapButtonStyle::Outlined,
            pressed: false,
        }
    }

    pub const fn style(mut self, style: BitmapButtonStyle) -> Self {
        self.style = style;
        self
    }

    pub fn set_pressed(&mut self, pressed: bool) {
        self.pressed = pressed;
    }

    pub fn is_pressed(&self) -> bool {
        self.pressed
    }

    pub fn draw(&self, strip: &mut StripBuffer) -> Result<(), Infallible> {
        let (bg, fg) = if self.pressed {
            (BinaryColor::On, BinaryColor::Off)
        } else {
            (BinaryColor::Off, BinaryColor::On)
        };

        let rect = self.region.to_rect();

        match self.style {
            BitmapButtonStyle::Outlined => {
                rect.into_styled(PrimitiveStyle::with_fill(bg))
                    .draw(strip)?;
                rect.into_styled(PrimitiveStyle::with_stroke(fg, 2))
                    .draw(strip)?;
            }
            BitmapButtonStyle::Filled => {
                rect.into_styled(PrimitiveStyle::with_fill(fg))
                    .draw(strip)?;
            }
            BitmapButtonStyle::Rounded(radius) => {
                let rounded =
                    RoundedRectangle::new(rect, CornerRadii::new(Size::new(radius, radius)));
                rounded
                    .into_styled(PrimitiveStyle::with_fill(bg))
                    .draw(strip)?;
                rounded
                    .into_styled(PrimitiveStyle::with_stroke(fg, 2))
                    .draw(strip)?;
            }
        }

        if self.label.is_empty() {
            return Ok(());
        }

        // For Filled buttons the text colour is the background colour (inverted
        // relative to the fill), matching the mono Button behaviour.
        let text_fg = match self.style {
            BitmapButtonStyle::Filled => bg,
            _ => fg,
        };

        // Centre text within the region.
        let text_w = self.font.measure_str(self.label) as u32;
        let text_h = self.font.line_height as u32;
        let top_left = Alignment::Center.position(self.region, Size::new(text_w, text_h));
        let cx = top_left.x;
        let baseline = top_left.y + self.font.ascent as i32;

        self.font
            .draw_str_fg(strip, self.label, text_fg, cx, baseline);

        Ok(())
    }
}
