// Static and dynamic text labels for e-paper
// Label borrows its text; DynamicLabel<N> owns a fixed buffer
// and implements core::fmt::Write for formatted output.

use embedded_graphics::{
    mono_font::{MonoFont, MonoTextStyle},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::PrimitiveStyle,
    text::Text,
};

use super::widget::{Alignment, Region, Widget, WidgetState};

pub struct Label<'a> {
    region: Region,
    text: &'a str,
    font: &'static MonoFont<'static>,
    alignment: Alignment,
    inverted: bool,
    state: WidgetState,
}

impl<'a> Label<'a> {
    pub fn new(region: Region, text: &'a str, font: &'static MonoFont<'static>) -> Self {
        Self {
            region,
            text,
            font,
            alignment: Alignment::CenterLeft,
            inverted: false,
            state: WidgetState::Dirty,
        }
    }

    pub const fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    pub const fn inverted(mut self, inverted: bool) -> Self {
        self.inverted = inverted;
        self
    }

    pub fn set_text(&mut self, text: &'a str) {
        if self.text != text {
            self.text = text;
            self.state = WidgetState::Dirty;
        }
    }

    pub fn set_inverted(&mut self, inverted: bool) {
        if self.inverted != inverted {
            self.inverted = inverted;
            self.state = WidgetState::Dirty;
        }
    }

    fn text_size(&self) -> Size {
        let char_width = self.font.character_size.width + self.font.character_spacing;
        let width = self.text.len() as u32 * char_width;
        let height = self.font.character_size.height;
        Size::new(width, height)
    }
}

impl<'a> Widget for Label<'a> {
    fn bounds(&self) -> Region {
        self.region
    }

    fn draw<D>(&self, display: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        let (bg, fg) = if self.inverted {
            (BinaryColor::On, BinaryColor::Off)
        } else {
            (BinaryColor::Off, BinaryColor::On)
        };

        self.region
            .to_rect()
            .into_styled(PrimitiveStyle::with_fill(bg))
            .draw(display)?;

        let text_size = self.text_size();
        let mut pos = self.alignment.position(self.region, text_size);

        pos.y += self.font.character_size.height as i32;

        let style = MonoTextStyle::new(self.font, fg);
        Text::new(self.text, pos, style).draw(display)?;

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

pub struct DynamicLabel<const N: usize> {
    region: Region,
    buffer: [u8; N],
    len: usize,
    font: &'static MonoFont<'static>,
    alignment: Alignment,
    inverted: bool,
    state: WidgetState,
}

impl<const N: usize> DynamicLabel<N> {
    pub fn new(region: Region, font: &'static MonoFont<'static>) -> Self {
        Self {
            region,
            buffer: [0u8; N],
            len: 0,
            font,
            alignment: Alignment::CenterLeft,
            inverted: false,
            state: WidgetState::Dirty,
        }
    }

    pub const fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    pub const fn inverted(mut self, inverted: bool) -> Self {
        self.inverted = inverted;
        self
    }

    pub fn set_text(&mut self, text: &str) {
        let bytes = text.as_bytes();
        let copy_len = bytes.len().min(N);
        self.buffer[..copy_len].copy_from_slice(&bytes[..copy_len]);
        self.len = copy_len;
        self.state = WidgetState::Dirty;
    }

    pub fn clear_text(&mut self) {
        self.len = 0;
        self.state = WidgetState::Dirty;
    }

    pub fn text(&self) -> &str {
        core::str::from_utf8(&self.buffer[..self.len]).unwrap_or("")
    }

    pub fn set_inverted(&mut self, inverted: bool) {
        if self.inverted != inverted {
            self.inverted = inverted;
            self.state = WidgetState::Dirty;
        }
    }

    fn text_size(&self) -> Size {
        let char_width = self.font.character_size.width + self.font.character_spacing;
        let width = self.len as u32 * char_width;
        let height = self.font.character_size.height;
        Size::new(width, height)
    }
}

impl<const N: usize> Widget for DynamicLabel<N> {
    fn bounds(&self) -> Region {
        self.region
    }

    fn draw<D>(&self, display: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        let (bg, fg) = if self.inverted {
            (BinaryColor::On, BinaryColor::Off)
        } else {
            (BinaryColor::Off, BinaryColor::On)
        };

        self.region
            .to_rect()
            .into_styled(PrimitiveStyle::with_fill(bg))
            .draw(display)?;

        let text_size = self.text_size();
        let mut pos = self.alignment.position(self.region, text_size);
        pos.y += self.font.character_size.height as i32;

        let style = MonoTextStyle::new(self.font, fg);
        Text::new(self.text(), pos, style).draw(display)?;

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

impl<const N: usize> core::fmt::Write for DynamicLabel<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let available = N - self.len;
        let copy_len = bytes.len().min(available);
        self.buffer[self.len..self.len + copy_len].copy_from_slice(&bytes[..copy_len]);
        self.len += copy_len;
        self.state = WidgetState::Dirty;
        Ok(())
    }
}
