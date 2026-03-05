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
    CONTENT_H, CONTENT_TOP, FOOTER_MARGIN, FOOTER_Y, FULL_CONTENT_W, HEADER_W, LARGE_MARGIN,
    LIST_ROW_GAP, LIST_ROW_H, LIST_ROW_STRIDE, LOADING_H, MENU_ROW_GAP, MENU_ROW_H,
    MENU_ROW_STRIDE, POSITION_OVERLAY_H, POSITION_OVERLAY_W, PROGRESS_H, SECTION_GAP, STANDARD_GAP,
    STANDARD_MARGIN, STATUS_W, STATUS_X, TITLE_Y, TITLE_Y_OFFSET,
};
pub use stack_fmt::{stack_fmt, StackFmt};
pub use statusbar::{free_stack_bytes, paint_stack, stack_high_water_mark, BAR_HEIGHT};
pub use widget::{
    draw_loading_indicator, draw_progress_bar, wrap_next, wrap_prev, Alignment, Region,
};

pub use crate::board::{SCREEN_H, SCREEN_W};
