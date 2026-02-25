// Strip-based rendering buffer
//
// Instead of holding a full 48KB framebuffer in SRAM, we render
// through a small strip buffer (~4KB) and stream each strip to
// the display controller via SPI.
// The display is divided into horizontal bands of physical rows.
// For each band:
//  1. Clear the strip buffer to white
//  2. Draw all widgets (DrawTarget clips to current band)
//  3. SPI transfer the strip data to display controller RAM
//  4. Reuse the buffer for the next band

use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::BinaryColor,
};

use super::display::{HEIGHT, Rotation, WIDTH};

// Physical rows per strip for full-page rendering.
// 480 / 40 = 12 strips, each 100 bytes/row × 40 rows = 4000 bytes.
pub const STRIP_ROWS: u16 = 40;
pub const PHYS_BYTES_PER_ROW: usize = (WIDTH as usize) / 8; // 100

pub const STRIP_BUF_SIZE: usize = PHYS_BYTES_PER_ROW * STRIP_ROWS as usize; // 4000
pub const STRIP_COUNT: u16 = HEIGHT / STRIP_ROWS; // 12

// A small rendering buffer that covers a physical rectangle of the display.
//
// Operates in two modes:
// - Full-width strips: For full-page rendering. Covers 800×40 physical
//  pixels (4000 bytes). Iterate 12 strips top-to-bottom.
// - Windowed: For partial refresh of small UI regions. Covers an
//  arbitrary physical rectangle that fits within STRIP_BUF_SIZE bytes.
pub struct StripBuffer {
    buf: [u8; STRIP_BUF_SIZE],
    rotation: Rotation,
    // Physical window this strip covers
    win_x: u16,
    win_y: u16,
    win_w: u16,
    win_h: u16,
    // Derived from win_w for indexing
    row_bytes: u16,
}

impl StripBuffer {
    pub const fn new() -> Self {
        Self {
            buf: [0xFF; STRIP_BUF_SIZE], // White
            rotation: Rotation::Deg270,
            win_x: 0,
            win_y: 0,
            win_w: WIDTH,
            win_h: STRIP_ROWS,
            row_bytes: (WIDTH / 8),
        }
    }

    // Configure for full-width strip rendering at the given strip index.
    // Clears the buffer to white.
    //
    // strip_idx: 0..STRIP_COUNT (physical row bands top-to-bottom)
    pub fn begin_strip(&mut self, rotation: Rotation, strip_idx: u16) {
        self.rotation = rotation;
        self.win_x = 0;
        self.win_y = strip_idx * STRIP_ROWS;
        self.win_w = WIDTH;
        self.win_h = STRIP_ROWS;
        self.row_bytes = PHYS_BYTES_PER_ROW as u16;

        // Clear to white
        self.buf[..STRIP_BUF_SIZE].fill(0xFF);
    }

    // Configure for an arbitrary physical window (partial refresh mode).
    // Region must be byte-aligned (x and w multiples of 8).
    // NOTE: Panics if the window doesn't fit in STRIP_BUF_SIZE.
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

    // Get the valid data bytes for SPI transfer.
    // Only the bytes covering the current window are returned.
    pub fn data(&self) -> &[u8] {
        let total = self.row_bytes as usize * self.win_h as usize;
        &self.buf[..total]
    }

    // Current window's physical origin and size.
    pub fn window(&self) -> (u16, u16, u16, u16) {
        (self.win_x, self.win_y, self.win_w, self.win_h)
    }

    pub const fn strip_count() -> u16 {
        STRIP_COUNT
    }

    // Max rows that fit in the buffer at a given window width.
    pub fn max_rows_for_width(width: u16) -> u16 {
        let rb = (width / 8) as usize;
        if rb == 0 {
            return 0;
        }
        (STRIP_BUF_SIZE / rb) as u16
    }

    // Transform logical coordinates to physical based on rotation.
    #[inline]
    fn to_physical(&self, lx: u16, ly: u16) -> (u16, u16) {
        match self.rotation {
            Rotation::Deg0 => (lx, ly),
            Rotation::Deg90 => (WIDTH - 1 - ly, lx),
            Rotation::Deg180 => (WIDTH - 1 - lx, HEIGHT - 1 - ly),
            Rotation::Deg270 => (ly, HEIGHT - 1 - lx),
        }
    }

    // set a pixel in the buffer using physical coordinates.
    // silently clips if outside current window.
    #[inline]
    fn set_pixel_physical(&mut self, px: u16, py: u16, black: bool) {
        // clip to window
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

// embedded-graphics integration

impl OriginDimensions for StripBuffer {
    // Report FULL logical display size.
    // Widgets think they're drawing to the entire screen;
    // the strip clips at the physical level.
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
            // Bounds check against logical dimensions
            if coord.x < 0 || coord.x >= log_w || coord.y < 0 || coord.y >= log_h {
                continue;
            }

            // Transform logical → physical, then clip to current strip
            let (px, py) = self.to_physical(coord.x as u16, coord.y as u16);
            self.set_pixel_physical(px, py, color == BinaryColor::On);
        }
        Ok(())
    }
}
