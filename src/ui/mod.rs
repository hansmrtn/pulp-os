//! UI primitives for e-paper displays

mod button;
mod label;
pub mod statusbar;
mod widget;

pub use button::{Button, ButtonStyle};
pub use label::{DynamicLabel, Label};
pub use statusbar::{BAR_HEIGHT, CONTENT_TOP, StatusBar, SystemStatus, free_stack_bytes};
pub use widget::{Alignment, Region, Widget, WidgetState};

use embedded_graphics::{pixelcolor::BinaryColor, prelude::*};

/// Extension trait for drawing widgets
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
