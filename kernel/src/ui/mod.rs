// widget primitives for 1-bit e-paper displays
//
// font-independent: Region, Alignment, stack measurement, StackFmt
// font-dependent widgets (BitmapLabel, QuickMenu, ButtonFeedback)
// live in the distro's apps::widgets module

pub mod layout;
pub mod stack_fmt;
pub mod statusbar;
mod widget;

pub use layout::{
    CONTENT_TOP, FULL_CONTENT_W, HEADER_W, LARGE_MARGIN, SECTION_GAP, TITLE_Y, TITLE_Y_OFFSET,
};
pub use stack_fmt::{stack_fmt, StackFmt};
pub use statusbar::{free_stack_bytes, paint_stack, stack_high_water_mark, BAR_HEIGHT};
pub use widget::{
    draw_loading_indicator, draw_progress_bar, wrap_next, wrap_prev, Alignment, Region,
};

pub use crate::board::{SCREEN_H, SCREEN_W};
