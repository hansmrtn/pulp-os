// strip-based rendering buffer for e-paper
// 4 KB strip instead of 48 KB framebuffer; display split into horizontal bands
// widgets draw to logical coords, clipped here

use embedded_graphics_core::{
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::BinaryColor,
    primitives::Rectangle,
    Pixel,
};

use super::ssd1677::{Rotation, HEIGHT, WIDTH};
use crate::ui::Region;

pub const STRIP_ROWS: u16 = 40;
pub const PHYS_BYTES_PER_ROW: usize = (WIDTH as usize) / 8;

pub const STRIP_BUF_SIZE: usize = PHYS_BYTES_PER_ROW * STRIP_ROWS as usize;
pub const STRIP_COUNT: u16 = HEIGHT / STRIP_ROWS;

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

    pub fn begin_window(&mut self, rotation: Rotation, x: u16, y: u16, w: u16, mut h: u16) {
        let rb = (w / 8) as usize;
        if rb == 0 {
            self.win_w = 0;
            self.win_h = 0;
            self.row_bytes = 0;
            return;
        }
        let max_h = (STRIP_BUF_SIZE / rb) as u16;
        if h > max_h {
            log::warn!(
                "begin_window: {}x{} exceeds strip buf, clamping h -> {}",
                w,
                h,
                max_h
            );
            h = max_h;
        }
        let total = rb * h as usize;

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

    pub fn data_mut(&mut self) -> &mut [u8] {
        let total = self.row_bytes as usize * self.win_h as usize;
        &mut self.buf[..total]
    }

    pub fn window(&self) -> (u16, u16, u16, u16) {
        (self.win_x, self.win_y, self.win_w, self.win_h)
    }

    pub fn logical_window(&self) -> Region {
        match self.rotation {
            Rotation::Deg0 => Region::new(self.win_x, self.win_y, self.win_w, self.win_h),
            Rotation::Deg90 => Region::new(
                self.win_y,
                WIDTH - self.win_x - self.win_w,
                self.win_h,
                self.win_w,
            ),
            Rotation::Deg180 => Region::new(
                WIDTH - self.win_x - self.win_w,
                HEIGHT - self.win_y - self.win_h,
                self.win_w,
                self.win_h,
            ),
            Rotation::Deg270 => Region::new(
                HEIGHT - self.win_y - self.win_h,
                self.win_x,
                self.win_h,
                self.win_w,
            ),
        }
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

    fn to_physical(&self, lx: u16, ly: u16) -> (u16, u16) {
        match self.rotation {
            Rotation::Deg0 => (lx, ly),
            Rotation::Deg90 => (WIDTH - 1 - ly, lx),
            Rotation::Deg180 => (WIDTH - 1 - lx, HEIGHT - 1 - ly),
            Rotation::Deg270 => (ly, HEIGHT - 1 - lx),
        }
    }

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

    #[allow(clippy::too_many_arguments)]
    pub fn blit_1bpp(
        &mut self,
        bitmaps: &[u8],
        offset: usize,
        w: usize,
        h: usize,
        stride: usize,
        gx: i32,
        gy: i32,
        black: bool,
    ) {
        if w == 0 || h == 0 || offset + stride * h > bitmaps.len() {
            return;
        }
        match self.rotation {
            Rotation::Deg270 => self.blit_1bpp_270(bitmaps, offset, w, h, stride, gx, gy, black),
            _ => self.blit_1bpp_generic(bitmaps, offset, w, h, stride, gx, gy, black),
        }
    }

    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn blit_1bpp_270(
        &mut self,
        bitmaps: &[u8],
        offset: usize,
        w: usize,
        h: usize,
        stride: usize,
        gx: i32,
        gy: i32,
        black: bool,
    ) {
        // window bounds (wx, wy = origin; wx2, wy2 = extent; rb = row bytes)
        let wx = self.win_x as i32;
        let wy = self.win_y as i32;
        let wx2 = wx + self.win_w as i32;
        let wy2 = wy + self.win_h as i32;
        let rb = self.row_bytes as usize;

        // clip glyph rows (y axis) against physical-x window
        let y0 = (wx - gy).clamp(0, h as i32) as usize;
        let y1 = (wx2 - gy).clamp(0, h as i32) as usize;
        if y0 >= y1 {
            return;
        }

        // clip glyph cols (x axis) against physical-y window
        // phys_y = HEIGHT-1-gx-x, decreasing with x
        let x0 = (HEIGHT as i32 - gx - wy2).clamp(0, w as i32) as usize;
        let x1 = (HEIGHT as i32 - gx - wy).clamp(0, w as i32) as usize;
        if x0 >= x1 {
            return;
        }

        // buf_y for x=0: physical row offset into window
        let base_buf_y_i = HEIGHT as i32 - 1 - gx - wy;
        debug_assert!(base_buf_y_i >= 0, "blit_1bpp_270: base_buf_y underflow");
        debug_assert!(gy + y0 as i32 >= wx, "blit_1bpp_270: buf_x underflow");
        let base_buf_y = base_buf_y_i as usize;

        // loop order: x-outer (strip rows, sequential memory), y-inner
        // (strip columns, nearby bytes in same row). this is the opposite
        // of the source row-major order but gives much better cache locality
        // for the destination strip buffer writes.
        for x in x0..x1 {
            let src_byte_idx = x / 8;
            let src_bit = 1u8 << (7 - (x & 7));
            let dst_row_base = (base_buf_y - x) * rb;

            if black {
                for y in y0..y1 {
                    if bitmaps[offset + y * stride + src_byte_idx] & src_bit != 0 {
                        let buf_x = (gy + y as i32 - wx) as usize;
                        let byte_col = buf_x / 8;
                        let inv_mask = !(1u8 << (7 - (buf_x & 7)));
                        self.buf[dst_row_base + byte_col] &= inv_mask;
                    }
                }
            } else {
                for y in y0..y1 {
                    if bitmaps[offset + y * stride + src_byte_idx] & src_bit != 0 {
                        let buf_x = (gy + y as i32 - wx) as usize;
                        let byte_col = buf_x / 8;
                        let mask = 1u8 << (7 - (buf_x & 7));
                        self.buf[dst_row_base + byte_col] |= mask;
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn blit_1bpp_generic(
        &mut self,
        bitmaps: &[u8],
        offset: usize,
        w: usize,
        h: usize,
        stride: usize,
        gx: i32,
        gy: i32,
        black: bool,
    ) {
        let (lw, lh) = match self.rotation {
            Rotation::Deg0 | Rotation::Deg180 => (WIDTH as i32, HEIGHT as i32),
            Rotation::Deg90 | Rotation::Deg270 => (HEIGHT as i32, WIDTH as i32),
        };

        for y in 0..h {
            let ly = gy + y as i32;
            if ly < 0 || ly >= lh {
                continue;
            }
            let row = offset + y * stride;
            for x in 0..w {
                let lx = gx + x as i32;
                if lx < 0 || lx >= lw {
                    continue;
                }
                if bitmaps[row + x / 8] & (1 << (7 - (x & 7))) != 0 {
                    let (px, py) = self.to_physical(lx as u16, ly as u16);
                    self.set_pixel_physical(px, py, black);
                }
            }
        }
    }

    fn fill_physical_rect(&mut self, px0: u16, py0: u16, px1: u16, py1: u16, black: bool) {
        let cx0 = px0.max(self.win_x);
        let cx1 = px1.min(self.win_x + self.win_w);
        let cy0 = py0.max(self.win_y);
        let cy1 = py1.min(self.win_y + self.win_h);
        if cx0 >= cx1 || cy0 >= cy1 {
            return;
        }

        let lx0 = (cx0 - self.win_x) as usize;
        let lx1 = (cx1 - self.win_x) as usize;
        let ly0 = (cy0 - self.win_y) as usize;
        let ly1 = (cy1 - self.win_y) as usize;
        let rb = self.row_bytes as usize;

        let first_byte = lx0 / 8;
        let last_byte = (lx1 - 1) / 8;
        let first_mask: u8 = 0xFF >> (lx0 & 7);
        let last_mask: u8 = 0xFF << (7 - ((lx1 - 1) & 7));

        let (fill, edge_op): (u8, fn(&mut u8, u8)) = if black {
            (0x00, |b, m| *b &= !m)
        } else {
            (0xFF, |b, m| *b |= m)
        };

        for ly in ly0..ly1 {
            let row = ly * rb;
            if first_byte == last_byte {
                edge_op(&mut self.buf[row + first_byte], first_mask & last_mask);
            } else {
                edge_op(&mut self.buf[row + first_byte], first_mask);
                for b in first_byte + 1..last_byte {
                    self.buf[row + b] = fill;
                }
                edge_op(&mut self.buf[row + last_byte], last_mask);
            }
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

        let black = color == BinaryColor::On;

        match self.rotation {
            Rotation::Deg0 => {
                self.fill_physical_rect(lx0, ly0, lx1, ly1, black);
            }
            Rotation::Deg90 => {
                self.fill_physical_rect(WIDTH - ly1, lx0, WIDTH - ly0, lx1, black);
            }
            Rotation::Deg180 => {
                self.fill_physical_rect(
                    WIDTH - lx1,
                    HEIGHT - ly1,
                    WIDTH - lx0,
                    HEIGHT - ly0,
                    black,
                );
            }
            Rotation::Deg270 => {
                self.fill_physical_rect(ly0, HEIGHT - lx1, ly1, HEIGHT - lx0, black);
            }
        }
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
        let size = self.size();
        let log_w = size.width as i32;
        let log_h = size.height as i32;

        for color in colors {
            if x >= 0 && x < log_w && y >= 0 && y < log_h {
                let (px, py) = self.to_physical(x as u16, y as u16);
                self.set_pixel_physical(px, py, color == BinaryColor::On);
            }
            x += 1;
            if x >= x_end {
                x = area.top_left.x;
                y += 1;
            }
        }
        Ok(())
    }
}
