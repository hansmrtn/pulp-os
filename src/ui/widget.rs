// Region geometry, alignment, and base widget trait
// All coordinates are logical (rotation aware). x/w should be 8 aligned
// for partial refresh to avoid byte boundary fixups on the controller.

use embedded_graphics::{
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
};

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

    pub fn from_rect(rect: Rectangle) -> Self {
        Self {
            x: rect.top_left.x.max(0) as u16,
            y: rect.top_left.y.max(0) as u16,
            w: rect.size.width as u16,
            h: rect.size.height as u16,
        }
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

    pub fn center(self) -> Point {
        Point::new((self.x + self.w / 2) as i32, (self.y + self.h / 2) as i32)
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

    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x as i32
            && point.x < (self.x + self.w) as i32
            && point.y >= self.y as i32
            && point.y < (self.y + self.h) as i32
    }

    pub fn inset(self, margin: u16) -> Self {
        Self {
            x: self.x + margin,
            y: self.y + margin,
            w: self.w.saturating_sub(margin * 2),
            h: self.h.saturating_sub(margin * 2),
        }
    }

    pub fn expand(self, margin: u16) -> Self {
        Self {
            x: self.x.saturating_sub(margin),
            y: self.y.saturating_sub(margin),
            w: self.w + margin * 2,
            h: self.h + margin * 2,
        }
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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum WidgetState {
    #[default]
    Dirty,
    Clean,
}

pub trait Widget {
    fn bounds(&self) -> Region;

    fn draw<D>(&self, display: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>;

    fn is_dirty(&self) -> bool {
        true
    }

    fn mark_clean(&mut self) {}

    fn mark_dirty(&mut self) {}

    fn clear<D>(&self, display: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        self.bounds()
            .to_rect()
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
            .draw(display)
    }

    fn refresh_bounds(&self) -> Region {
        self.bounds().align8()
    }
}
