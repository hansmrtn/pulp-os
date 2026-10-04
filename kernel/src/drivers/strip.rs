// strip-based rendering buffer for e-paper
// 4 KB strip instead of a 48 KB framebuffer; display split into horizontal bands
// widgets draw to logical coords, clipped here
//
// The byte/bit layout, rotation math, blits and fills live in
// pulp_board_logic::strip::StripCore (host-tested, shared with the OnePage C61
// image). This file only adds the embedded-graphics DrawTarget glue and the
// `ui::Region` view, so X4 behavior is the same code that the host tests cover.

use core::ops::{Deref, DerefMut};

use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::BinaryColor,
    primitives::Rectangle,
};
use pulp_board_logic::strip::StripCore;
pub use pulp_board_logic::strip::{PHYS_BYTES_PER_ROW, STRIP_BUF_SIZE, STRIP_COUNT, STRIP_ROWS};

use crate::ui::Region;
use pulp_board_logic::ssd1677::Rotation;

pub struct StripBuffer(StripCore);

// begin_window, data, data_mut, window, begin_strip, blit_1bpp, ... come from
// StripCore through Deref; only the Region view and begin_window's warning
// are defined here.
impl Deref for StripBuffer {
    type Target = StripCore;

    #[inline]
    fn deref(&self) -> &StripCore {
        &self.0
    }
}

impl DerefMut for StripBuffer {
    #[inline]
    fn deref_mut(&mut self) -> &mut StripCore {
        &mut self.0
    }
}

impl StripBuffer {
    pub const fn new() -> Self {
        Self(StripCore::new())
    }

    pub fn begin_window(&mut self, rotation: Rotation, x: u16, y: u16, w: u16, h: u16) {
        if self.0.begin_window(rotation, x, y, w, h) {
            log::warn!(
                "begin_window: {}x{} exceeds strip buf, clamping h -> {}",
                w,
                h,
                StripCore::max_rows_for_width(w)
            );
        }
    }

    pub fn logical_window(&self) -> Region {
        let (x, y, w, h) = self.0.logical_window_xywh();
        Region::new(x, y, w, h)
    }

    pub const fn strip_count() -> u16 {
        STRIP_COUNT
    }

    pub fn max_rows_for_width(width: u16) -> u16 {
        StripCore::max_rows_for_width(width)
    }
}

impl Default for StripBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl OriginDimensions for StripBuffer {
    fn size(&self) -> Size {
        let (w, h) = self.0.logical_size();
        Size::new(w as u32, h as u32)
    }
}

impl DrawTarget for StripBuffer {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(coord, color) in pixels {
            self.0
                .draw_pixel_logical(coord.x, coord.y, color == BinaryColor::On);
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let size = self.size();
        let sw = size.width as u16;
        let sh = size.height as u16;

        let lx0 = (area.top_left.x.max(0) as u16).min(sw);
        let ly0 = (area.top_left.y.max(0) as u16).min(sh);
        let lx1 = ((area.top_left.x.saturating_add(area.size.width as i32)).max(0) as u16).min(sw);
        let ly1 = ((area.top_left.y.saturating_add(area.size.height as i32)).max(0) as u16).min(sh);
        if lx0 >= lx1 || ly0 >= ly1 {
            return Ok(());
        }

        self.0
            .fill_logical_rect(lx0, ly0, lx1, ly1, color == BinaryColor::On);
        Ok(())
    }

    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Self::Color>,
    {
        let w = area.size.width as i32;
        if w == 0 {
            return Ok(());
        }
        let mut x = area.top_left.x;
        let mut y = area.top_left.y;
        let x_end = x + w;

        for color in colors {
            self.0.draw_pixel_logical(x, y, color == BinaryColor::On);
            x += 1;
            if x >= x_end {
                x = area.top_left.x;
                y += 1;
            }
        }
        Ok(())
    }
}
