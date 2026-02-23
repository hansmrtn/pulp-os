//! UI primitives for e-paper displays
//!
//! This module provides rotation-aware widgets that handle their own
//! partial refresh regions automatically.
//!
//! # Example
//!
//! ```ignore
//! use pulp_os::ui::{Region, Widget, Label, Button};
//! use embedded_graphics::mono_font::ascii::FONT_10X20;
//!
//! // Define regions (8-pixel aligned for partial refresh)
//! const TITLE_REGION: Region = Region::new(16, 8, 200, 32);
//! const BTN_REGION: Region = Region::new(16, 48, 96, 40);
//!
//! // Create widgets
//! let title = Label::new(TITLE_REGION, "Hello!", &FONT_10X20);
//! let mut btn = Button::new(BTN_REGION, "Click", &FONT_10X20);
//!
//! // Draw and refresh
//! title.draw(&mut display).unwrap();
//! btn.draw(&mut display).unwrap();
//! display.refresh_full(&mut delay);
//!
//! // Later, update just the button
//! btn.set_pressed(true);
//! btn.draw(&mut display).unwrap();
//! let r = btn.refresh_bounds();
//! display.refresh_partial(r.x, r.y, r.w, r.h, &mut delay);
//! ```

mod widget;
mod label;
mod button;
// mod progress;

pub use widget::{Region, Widget, Alignment, WidgetState};
pub use label::{Label, DynamicLabel};
pub use button::{Button, ButtonStyle};
// pub use progress::{ProgressBar, BatteryIndicator, Orientation};

use embedded_graphics::{pixelcolor::BinaryColor, prelude::*};

/// Extension trait for drawing and refreshing widgets
///
/// This trait provides convenience methods for displays that support
/// partial refresh. Import it to use `draw_widget` and `refresh_widget`.
pub trait WidgetExt<D>
where
    D: DrawTarget<Color = BinaryColor>,
{
    /// Draw a widget to the display (does not refresh)
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

