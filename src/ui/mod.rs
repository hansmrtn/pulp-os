// Widget toolkit for 1-bit e-paper displays
// Region based layout, dirty tracking, strip-buffered rendering.

mod button;
mod label;
pub mod statusbar;
mod widget;

pub use button::{Button, ButtonStyle};
pub use label::{DynamicLabel, Label};
pub use statusbar::{BAR_HEIGHT, CONTENT_TOP, StatusBar, SystemStatus, free_stack_bytes};
pub use widget::{Alignment, Region, Widget, WidgetState};

use embedded_graphics::{pixelcolor::BinaryColor, prelude::*};

/// Full logical screen region (480×800 after Deg270 rotation).
/// Used by apps that need to repaint everything without forcing
/// a full hardware refresh (which clears ghosting but flashes).
pub const SCREEN_REGION: Region = Region::new(
    0,
    0,
    crate::board::display::HEIGHT, // logical width  = physical height
    crate::board::display::WIDTH,  // logical height = physical width
);

pub trait WidgetExt<D>
where
    D: DrawTarget<Color = BinaryColor>,
{
    fn draw_widget<W: Widget>(&mut self, widget: &W) -> Result<(), D::Error>;
}

impl<D> WidgetExt<D> for D
where
    D: DrawTarget<Color = BinaryColor>,
{
    fn draw_widget<W: Widget>(&mut self, widget: &W) -> Result<(), D::Error> {
        widget.draw(self)
    }
}
