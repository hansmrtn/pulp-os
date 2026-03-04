// widget primitives for 1-bit e-paper displays
//
// font-independent: Region, Alignment, stack measurement, StackFmt.
// font-dependent widgets (BitmapLabel, QuickMenu, ButtonFeedback)
// live in the distro's apps::widgets module.

pub mod stack_fmt;
pub mod statusbar;
mod widget;

pub use stack_fmt::{StackFmt, stack_fmt};
pub use statusbar::{
    BAR_HEIGHT, CONTENT_TOP, free_stack_bytes, paint_stack, stack_high_water_mark,
};
pub use widget::{Alignment, Region, draw_progress_bar, wrap_next, wrap_prev};

pub use crate::board::{SCREEN_H, SCREEN_W};
