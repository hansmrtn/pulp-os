// Widget toolkit for 1-bit e-paper displays.
// BitmapLabel/BitmapDynLabel with inverted highlight for selection.
// Region-based layout, strip-buffered rendering.

mod bitmap_label;
pub mod button_feedback;
pub mod quick_menu;
pub mod statusbar;
mod widget;

pub use bitmap_label::{BitmapDynLabel, BitmapLabel};
pub use button_feedback::ButtonFeedback;
pub use quick_menu::QuickMenu;
pub use statusbar::{
    BAR_HEIGHT, CONTENT_TOP, StatusBar, SystemStatus, free_stack_bytes, paint_stack,
    stack_high_water_mark,
};
pub use widget::{Alignment, Region, wrap_next, wrap_prev};

// full logical screen region (480x800 after Deg270 rotation)
pub const SCREEN_REGION: Region = Region::new(
    0,
    0,
    crate::drivers::ssd1677::HEIGHT, // logical width = physical height
    crate::drivers::ssd1677::WIDTH,  // logical height = physical width
);
