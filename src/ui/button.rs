//! Button widget for interactive UI elements

use embedded_graphics::{
    mono_font::MonoFont,
    mono_font::MonoTextStyle,
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{CornerRadii, PrimitiveStyle, Rectangle, RoundedRectangle},
    text::Text,
};

use super::widget::{Alignment, Region, Widget, WidgetState};

/// Button visual style
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ButtonStyle {
    /// Simple rectangle outline
    #[default]
    Outlined,
    /// Filled rectangle
    Filled,
    /// Rounded corners (specify radius)
    Rounded(u32),
}

/// A clickable button widget
pub struct Button<'a> {
    region: Region,
    label: &'a str,
    font: &'static MonoFont<'static>,
    style: ButtonStyle,
    pressed: bool,
    state: WidgetState,
}

impl<'a> Button<'a> {
    pub fn new(region: Region, label: &'a str, font: &'static MonoFont<'static>) -> Self {
        Self {
            region,
            label,
            font,
            style: ButtonStyle::Outlined,
            pressed: false,
            state: WidgetState::Dirty,
        }
    }

    pub const fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    pub fn set_pressed(&mut self, pressed: bool) {
        if self.pressed != pressed {
            self.pressed = pressed;
            self.state = WidgetState::Dirty;
        }
    }

    pub fn is_pressed(&self) -> bool {
        self.pressed
    }

    pub fn toggle(&mut self) {
        self.pressed = !self.pressed;
        self.state = WidgetState::Dirty;
    }

    /// Check if a point is within this button's bounds
    pub fn contains(&self, point: Point) -> bool {
        self.region.contains(point)
    }

    fn text_size(&self) -> Size {
        let char_width = self.font.character_size.width + self.font.character_spacing;
        let width = self.label.len() as u32 * char_width;
        let height = self.font.character_size.height;
        Size::new(width, height)
    }
}

impl<'a> Widget for Button<'a> {
    fn bounds(&self) -> Region {
        self.region
    }

    fn draw<D>(&self, display: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        // When pressed, invert colors
        let (bg, fg) = if self.pressed {
            (BinaryColor::On, BinaryColor::Off)
        } else {
            (BinaryColor::Off, BinaryColor::On)
        };

        let rect = self.region.to_rect();

        // Draw button background/border based on style
        match self.style {
            ButtonStyle::Outlined => {
                // Clear background
                rect.into_styled(PrimitiveStyle::with_fill(bg))
                    .draw(display)?;
                // Draw border
                rect.into_styled(PrimitiveStyle::with_stroke(fg, 2))
                    .draw(display)?;
            }
            ButtonStyle::Filled => {
                rect.into_styled(PrimitiveStyle::with_fill(fg))
                    .draw(display)?;
            }
            ButtonStyle::Rounded(radius) => {
                let rounded =
                    RoundedRectangle::new(rect, CornerRadii::new(Size::new(radius, radius)));
                // Clear background
                rounded
                    .into_styled(PrimitiveStyle::with_fill(bg))
                    .draw(display)?;
                // Draw border
                rounded
                    .into_styled(PrimitiveStyle::with_stroke(fg, 2))
                    .draw(display)?;
            }
        }

        // Calculate centered text position
        let text_size = self.text_size();
        let mut pos = Alignment::Center.position(self.region, text_size);
        pos.y += self.font.character_size.height as i32;

        // For filled style, text color is inverted
        let text_color = match self.style {
            ButtonStyle::Filled => bg,
            _ => fg,
        };

        // Draw label
        let style = MonoTextStyle::new(self.font, text_color);
        Text::new(self.label, pos, style).draw(display)?;

        Ok(())
    }

    fn is_dirty(&self) -> bool {
        self.state == WidgetState::Dirty
    }

    fn mark_clean(&mut self) {
        self.state = WidgetState::Clean;
    }

    fn mark_dirty(&mut self) {
        self.state = WidgetState::Dirty;
    }
}
