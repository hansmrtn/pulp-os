// Proportional-font button widget; bitmap pipeline equivalent of button.rs.

use core::convert::Infallible;

use embedded_graphics::{
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{CornerRadii, PrimitiveStyle, RoundedRectangle},
};

use super::widget::{Alignment, Region};
use crate::drivers::strip::StripBuffer;
use crate::fonts::bitmap::BitmapFont;

pub const DEFAULT_RADIUS: u32 = 8;

const TEXT_PAD_X: u16 = 10; // inner horizontal padding each side

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BitmapButtonStyle {
    Outlined,
    Filled,
    Rounded(u32),
}

impl Default for BitmapButtonStyle {
    fn default() -> Self {
        BitmapButtonStyle::Rounded(DEFAULT_RADIUS)
    }
}

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
            style: BitmapButtonStyle::default(),
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
        if !self.region.intersects(strip.logical_window()) {
            return Ok(());
        }

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

        // Filled buttons invert text colour relative to fill, matching Button behaviour.
        let text_fg = match self.style {
            BitmapButtonStyle::Filled => bg,
            _ => fg,
        };

        // Constrain text measurement to the padded inner width so long labels
        // don't visually collide with the rounded border corners.
        let inner_w = self.region.w.saturating_sub(TEXT_PAD_X * 2) as u32;
        let text_w = (self.font.measure_str(self.label) as u32).min(inner_w);
        let text_h = self.font.line_height as u32;

        // Centre text within the full region; the min() above nudges it inward
        // if it would otherwise overflow.
        let inner_region = Region::new(
            self.region.x + TEXT_PAD_X,
            self.region.y,
            self.region.w.saturating_sub(TEXT_PAD_X * 2),
            self.region.h,
        );
        let top_left = Alignment::Center.position(inner_region, Size::new(text_w, text_h));
        let baseline = top_left.y + self.font.ascent as i32;

        self.font
            .draw_str_fg(strip, self.label, text_fg, top_left.x, baseline);

        Ok(())
    }
}
