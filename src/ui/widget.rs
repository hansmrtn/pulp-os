//! Widgets are self-contained UI elements that know their bounds and can
//! draw themselves. They work in logical coordinates (rotation-aware).
use embedded_graphics::{
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
};

/// A rectangular region in logical coordinates.
#[derive(Clone, Copy, Debug, Default)]
pub struct Region {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Region {
    /// Create a new region. X and W should be 8-pixel aligned for best performance.
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    /// Create from embedded-graphics Rectangle
    pub fn from_rect(rect: Rectangle) -> Self {
        Self {
            x: rect.top_left.x.max(0) as u16,
            y: rect.top_left.y.max(0) as u16,
            w: rect.size.width as u16,
            h: rect.size.height as u16,
        }
    }

    /// Convert to embedded-graphics Rectangle
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

    /// Align X to 8-pixel boundary (required for partial refresh)
    pub fn align8(self) -> Self {
        let aligned_x = (self.x / 8) * 8;
        let extra = self.x - aligned_x;
        Self {
            x: aligned_x,
            y: self.y,
            w: ((self.w + extra + 7) / 8) * 8, // Round up width to compensate
            h: self.h,
        }
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

/// Text/content alignment within a widget
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
    /// Calculate position for content of given size within a region
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

/// Widget state for tracking if redraw is needed
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum WidgetState {
    /// Widget needs to be redrawn
    #[default]
    Dirty,
    /// Widget is up to date
    Clean,
}

/// Core widget trait for UI elements
///
/// Widgets are self-contained UI components that:
/// - Know their bounds (region)
/// - Can draw themselves to any DrawTarget
/// - Track dirty state for efficient updates
pub trait Widget {
    /// Get the widget's bounding region (in logical coordinates)
    fn bounds(&self) -> Region;

    /// Draw the widget to a display
    fn draw<D>(&self, display: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>;

    /// Check if widget needs redraw
    fn is_dirty(&self) -> bool {
        true // Default: always redraw
    }

    /// Mark widget as clean (called after draw)
    fn mark_clean(&mut self) {
        // Default: no-op
    }

    /// Mark widget as needing redraw
    fn mark_dirty(&mut self) {
        // Default: no-op
    }

    /// Clear the widget's region to background color
    fn clear<D>(&self, display: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        self.bounds()
            .to_rect()
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
            .draw(display)
    }

    /// Get the 8-pixel aligned bounds for partial refresh
    fn refresh_bounds(&self) -> Region {
        self.bounds().align8()
    }
}

pub struct RectWidget {
    region: Region,
    filled: bool,
    inverted: bool,
    state: WidgetState,
}

impl RectWidget {
    pub const fn new(region: Region) -> Self {
        Self {
            region,
            filled: true,
            inverted: false,
            state: WidgetState::Dirty,
        }
    }

    pub const fn filled(mut self, filled: bool) -> Self {
        self.filled = filled;
        self
    }

    pub const fn inverted(mut self, inverted: bool) -> Self {
        self.inverted = inverted;
        self
    }

    pub fn set_inverted(&mut self, inverted: bool) {
        if self.inverted != inverted {
            self.inverted = inverted;
            self.state = WidgetState::Dirty;
        }
    }

    pub fn toggle(&mut self) {
        self.inverted = !self.inverted;
        self.state = WidgetState::Dirty;
    }
}

impl Widget for RectWidget {
    fn bounds(&self) -> Region {
        self.region
    }

    fn draw<D>(&self, display: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        let color = if self.inverted {
            BinaryColor::On
        } else {
            BinaryColor::Off
        };

        if self.filled {
            self.region
                .to_rect()
                .into_styled(PrimitiveStyle::with_fill(color))
                .draw(display)
        } else {
            self.region
                .to_rect()
                .into_styled(PrimitiveStyle::with_stroke(color, 1))
                .draw(display)
        }
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
