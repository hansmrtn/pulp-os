// region geometry and alignment helpers, progress bar drawing

use embedded_graphics::{
    pixelcolor::BinaryColor, prelude::*, primitives::PrimitiveStyle, primitives::Rectangle,
};

use crate::drivers::strip::StripBuffer;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Region {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Region {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    pub fn to_rect(self) -> Rectangle {
        Rectangle::new(
            Point::new(self.x as i32, self.y as i32),
            Size::new(self.w as u32, self.h as u32),
        )
    }

    pub fn top_left(self) -> Point {
        Point::new(self.x as i32, self.y as i32)
    }

    pub fn align8(self) -> Self {
        let aligned_x = (self.x / 8) * 8;
        let extra = self.x - aligned_x;
        Self {
            x: aligned_x,
            y: self.y,
            w: (self.w + extra).div_ceil(8) * 8,
            h: self.h,
        }
    }

    pub fn union(self, other: Region) -> Self {
        let x1 = self.x.min(other.x);
        let y1 = self.y.min(other.y);
        let x2 = (self.x + self.w).max(other.x + other.w);
        let y2 = (self.y + self.h).max(other.y + other.h);
        Self {
            x: x1,
            y: y1,
            w: x2 - x1,
            h: y2 - y1,
        }
    }

    pub fn intersects(self, other: Region) -> bool {
        self.x < other.x + other.w
            && self.x + self.w > other.x
            && self.y < other.y + other.h
            && self.y + self.h > other.y
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Alignment {
    #[default]
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Alignment {
    pub fn position(self, region: Region, content_size: Size) -> Point {
        let cw = content_size.width as i32;
        let ch = content_size.height as i32;
        let rx = region.x as i32;
        let ry = region.y as i32;
        let rw = region.w as i32;
        let rh = region.h as i32;

        match self {
            Alignment::TopLeft => Point::new(rx, ry),
            Alignment::TopCenter => Point::new(rx + (rw - cw) / 2, ry),
            Alignment::TopRight => Point::new(rx + rw - cw, ry),
            Alignment::CenterLeft => Point::new(rx, ry + (rh - ch) / 2),
            Alignment::Center => Point::new(rx + (rw - cw) / 2, ry + (rh - ch) / 2),
            Alignment::CenterRight => Point::new(rx + rw - cw, ry + (rh - ch) / 2),
            Alignment::BottomLeft => Point::new(rx, ry + rh - ch),
            Alignment::BottomCenter => Point::new(rx + (rw - cw) / 2, ry + rh - ch),
            Alignment::BottomRight => Point::new(rx + rw - cw, ry + rh - ch),
        }
    }
}

#[inline]
pub fn wrap_next(current: usize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    if current + 1 >= count { 0 } else { current + 1 }
}

#[inline]
pub fn wrap_prev(current: usize, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    if current == 0 { count - 1 } else { current - 1 }
}

// horizontal progress bar for 1-bit e-paper.
// draws a 1px black border around the full track and fills
// proportionally from the left. pct is clamped to 0..=100.
// region should be at least 4px wide and 4px tall.
pub fn draw_progress_bar(strip: &mut StripBuffer, region: Region, pct: u8) {
    let pct = pct.min(100) as u32;

    // clear region
    region
        .to_rect()
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
        .draw(strip)
        .unwrap();

    // 1px border shows full extent even at 0%
    region
        .to_rect()
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(strip)
        .unwrap();

    // filled portion inside the border
    if pct > 0 && region.w > 2 && region.h > 2 {
        let inner_w = (region.w - 2) as u32;
        let fill_w = (inner_w * pct / 100).max(1);
        Rectangle::new(
            Point::new((region.x + 1) as i32, (region.y + 1) as i32),
            Size::new(fill_w, (region.h - 2) as u32),
        )
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(strip)
        .unwrap();
    }
}
