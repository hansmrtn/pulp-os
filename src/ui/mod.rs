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

// full logical screen region (480×800 after Deg270 rotation)
pub const SCREEN_REGION: Region = Region::new(
    0,
    0,
    crate::drivers::ssd1677::HEIGHT, // logical width  = physical height
    crate::drivers::ssd1677::WIDTH,  // logical height = physical width
);
