// Strip based rendering buffer for e-paper
//
// Renders through a 4KB strip instead of a full 48KB framebuffer.
// The display is split into horizontal bands; each band is cleared,
// drawn into by all visible widgets, then sent over SPI. Widgets
// always draw to full logical screen coords; clipping happens here.
//
// Two modes:
//   begin_strip()  -- full width, fixed height (full page refresh)
//   begin_window() -- arbitrary rect (partial refresh)

use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::BinaryColor,
};

use super::display::{HEIGHT, Rotation, WIDTH};

pub const STRIP_ROWS: u16 = 40; // 800/8 * 40 = 4000B per strip
pub const PHYS_BYTES_PER_ROW: usize = (WIDTH as usize) / 8;

pub const STRIP_BUF_SIZE: usize = PHYS_BYTES_PER_ROW * STRIP_ROWS as usize; // 4000
pub const STRIP_COUNT: u16 = HEIGHT / STRIP_ROWS; // 12

pub struct StripBuffer {
    buf: [u8; STRIP_BUF_SIZE],
    rotation: Rotation,
    win_x: u16,
    win_y: u16,
    win_w: u16,
    win_h: u16,
    row_bytes: u16,
}

impl StripBuffer {
    pub const fn new() -> Self {
        Self {
            buf: [0xFF; STRIP_BUF_SIZE],
            rotation: Rotation::Deg270,
            win_x: 0,
            win_y: 0,
            win_w: WIDTH,
            win_h: STRIP_ROWS,
            row_bytes: (WIDTH / 8),
        }
    }

    pub fn begin_strip(&mut self, rotation: Rotation, strip_idx: u16) {
        self.rotation = rotation;
        self.win_x = 0;
        self.win_y = strip_idx * STRIP_ROWS;
        self.win_w = WIDTH;
        self.win_h = STRIP_ROWS;
        self.row_bytes = PHYS_BYTES_PER_ROW as u16;

        self.buf[..STRIP_BUF_SIZE].fill(0xFF);
    }

    pub fn begin_window(&mut self, rotation: Rotation, x: u16, y: u16, w: u16, h: u16) {
        let rb = (w / 8) as usize;
        let total = rb * h as usize;
        assert!(
            total <= STRIP_BUF_SIZE,
            "partial region {}×{} = {} bytes exceeds strip buffer ({})",
            w,
            h,
            total,
            STRIP_BUF_SIZE,
        );

        self.rotation = rotation;
        self.win_x = x;
        self.win_y = y;
        self.win_w = w;
        self.win_h = h;
        self.row_bytes = rb as u16;

        self.buf[..total].fill(0xFF);
    }

    pub fn data(&self) -> &[u8] {
        let total = self.row_bytes as usize * self.win_h as usize;
        &self.buf[..total]
    }

    pub fn window(&self) -> (u16, u16, u16, u16) {
        (self.win_x, self.win_y, self.win_w, self.win_h)
    }

    pub const fn strip_count() -> u16 {
        STRIP_COUNT
    }

    pub fn max_rows_for_width(width: u16) -> u16 {
        let rb = (width / 8) as usize;
        if rb == 0 {
            return 0;
        }
        (STRIP_BUF_SIZE / rb) as u16
    }

    #[inline]
    fn to_physical(&self, lx: u16, ly: u16) -> (u16, u16) {
        match self.rotation {
            Rotation::Deg0 => (lx, ly),
            Rotation::Deg90 => (WIDTH - 1 - ly, lx),
            Rotation::Deg180 => (WIDTH - 1 - lx, HEIGHT - 1 - ly),
            Rotation::Deg270 => (ly, HEIGHT - 1 - lx),
        }
    }

    #[inline]
    fn set_pixel_physical(&mut self, px: u16, py: u16, black: bool) {
        if px < self.win_x || px >= self.win_x + self.win_w {
            return;
        }
        if py < self.win_y || py >= self.win_y + self.win_h {
            return;
        }

        let local_x = (px - self.win_x) as usize;
        let local_y = (py - self.win_y) as usize;
        let idx = (local_x / 8) + (local_y * self.row_bytes as usize);
        let bit = 7 - (local_x as u16 % 8);

        if black {
            self.buf[idx] &= !(1 << bit);
        } else {
            self.buf[idx] |= 1 << bit;
        }
    }
}

impl Default for StripBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl OriginDimensions for StripBuffer {
    fn size(&self) -> Size {
        match self.rotation {
            Rotation::Deg0 | Rotation::Deg180 => Size::new(WIDTH as u32, HEIGHT as u32),
            Rotation::Deg90 | Rotation::Deg270 => Size::new(HEIGHT as u32, WIDTH as u32),
        }
    }
}

impl DrawTarget for StripBuffer {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let size = self.size();
        let log_w = size.width as i32;
        let log_h = size.height as i32;

        for Pixel(coord, color) in pixels {
            if coord.x < 0 || coord.x >= log_w || coord.y < 0 || coord.y >= log_h {
                continue;
            }

            let (px, py) = self.to_physical(coord.x as u16, coord.y as u16);
            self.set_pixel_physical(px, py, color == BinaryColor::On);
        }
        Ok(())
    }
}
