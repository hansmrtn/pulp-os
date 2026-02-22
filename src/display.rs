extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;
use core::convert::Infallible;

use embedded_graphics_core::{
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::BinaryColor,
    prelude::Pixel,
    geometry::Point,
};
use esp_hal::delay::Delay;
use log::info;
use ssd1677::{RefreshMode, Region, Rotation, UpdateRegion};
use ssd1677::rotation::apply_rotation;

use crate::board::{DisplayHw, Epd, DISPLAY_HEIGHT, DISPLAY_WIDTH, FB_SIZE};

const FULL_REFRESH_INTERVAL: u32 = 10;

pub struct DisplayDriver {
    epd: Epd,
    buf: Vec<u8>,
    /// Number of fast/partial flushes since the last full refresh.
    fast_count: u32,
    dirty: bool,
}

impl DisplayDriver {
    pub fn new(hw: DisplayHw) -> Self {
        let buf = vec![0xFFu8; FB_SIZE]; // 0xFF = all white

        Self {
            epd: hw.epd,
            buf,
            fast_count: 0,
            dirty: false,
        }
    }

    pub fn clear_white(&mut self) {
        self.buf.fill(0xFF);
        self.dirty = true;
    }

    pub fn clear_black(&mut self) {
        self.buf.fill(0x00);
        self.dirty = true;
    }

    pub fn flush(&mut self, delay: &mut Delay) {
        if !self.dirty {
            return;
        }

        let mode = if self.fast_count >= FULL_REFRESH_INTERVAL {
            info!("[EPD] full refresh (ghost cleanup)");
            RefreshMode::Full
        } else {
            RefreshMode::Fast
        };

        self.flush_inner(mode, delay);
    }

    pub fn flush_with_mode(&mut self, mode: RefreshMode, delay: &mut Delay) {
        self.flush_inner(mode, delay);
    }

    pub fn flush_full(&mut self, delay: &mut Delay) {
        self.flush_inner(RefreshMode::Full, delay);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn fast_count(&self) -> u32 {
        self.fast_count
    }

    pub fn epd(&mut self) -> &mut Epd {
        &mut self.epd
    }

    pub fn buffer(&self) -> &[u8] {
        &self.buf
    }

    fn flush_inner(&mut self, mode: RefreshMode, delay: &mut Delay) {
        let region = Region::new(0, 0, DISPLAY_HEIGHT, DISPLAY_WIDTH);

        let update = UpdateRegion {
            region,
            black_buffer: &self.buf,
            red_buffer: &[],
            mode,
        };

        if let Err(_e) = self.epd.update_region(update, delay) {
            info!("[EPD] flush error");
        }

        match mode {
            RefreshMode::Full => self.fast_count = 0,
            _ => self.fast_count += 1,
        }

        self.dirty = false;
    }

    fn set_pixel(&mut self, x: u32, y: u32, on: bool) {
        let rotation = self.epd.rotation();

        // Physical (unrotated) dimensions — cols is the byte-row width.
        let dims = self.epd.dimensions();
        let width = dims.cols as u32;
        let height = dims.rows as u32;

        let (index, bit) = apply_rotation(x, y, width, height, rotation);

        if index >= self.buf.len() {
            return;
        }

        if on {
            // "On" = black = clear bit
            self.buf[index] &= !bit;
        } else {
            // "Off" = white = set bit
            self.buf[index] |= bit;
        }
    }
}

impl DrawTarget for DisplayDriver {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let sz = self.size();

        for Pixel(Point { x, y }, color) in pixels {
            if x < 0 || y < 0 {
                continue;
            }
            let x = x as u32;
            let y = y as u32;
            if x >= sz.width || y >= sz.height {
                continue;
            }
            self.set_pixel(x, y, color.is_on());
            self.dirty = true;
        }

        Ok(())
    }
}

impl OriginDimensions for DisplayDriver {
    fn size(&self) -> Size {
        let rotated = self.epd.config().rotated_dimensions();
        Size::new(rotated.cols as u32, rotated.rows as u32)
    }
}
