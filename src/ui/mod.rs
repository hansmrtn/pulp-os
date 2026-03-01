// Widget toolkit for 1-bit e-paper displays.
// BitmapLabel/BitmapDynLabel with inverted highlight for selection.
// Region-based layout, strip-buffered rendering.

mod bitmap_label;
pub mod button_feedback;
pub mod quick_menu;
pub mod stack_fmt;
pub mod statusbar;
mod widget;

pub use bitmap_label::{BitmapDynLabel, BitmapLabel};
pub use button_feedback::{BUTTON_BAR_H, ButtonFeedback};
pub use quick_menu::QuickMenu;
pub use stack_fmt::{StackFmt, stack_fmt};
pub use statusbar::{
    BAR_HEIGHT, CONTENT_TOP, StatusBar, SystemStatus, free_stack_bytes, paint_stack,
    stack_high_water_mark,
};
pub use widget::{Alignment, Region, wrap_next, wrap_prev};

pub use crate::board::{SCREEN_H, SCREEN_W};
