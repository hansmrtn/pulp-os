// common layout constants for UI rendering
//
// centralizes magic layout values used across apps and widgets.
// some of these may become runtime-configurable settings in the
// future (e.g., margin sizes based on font size preferences).

use super::statusbar::BAR_HEIGHT;
use crate::board::{SCREEN_H, SCREEN_W};

// ── Content area ────────────────────────────────────────────────

/// Top of the content area (below status bar).
pub const CONTENT_TOP: u16 = BAR_HEIGHT;

/// Height of the content area (screen minus status bar).
pub const CONTENT_H: u16 = SCREEN_H - BAR_HEIGHT;

// ── Standard spacing ────────────────────────────────────────────

/// Standard margin for content edges (left/right).
pub const STANDARD_MARGIN: u16 = 8;

/// Large margin for content edges (used in some UIs).
pub const LARGE_MARGIN: u16 = 16;

/// Standard gap between items.
pub const STANDARD_GAP: u16 = 4;

/// Larger gap between sections or after headers.
pub const SECTION_GAP: u16 = 8;

// ── Title/header layout ─────────────────────────────────────────

/// Y offset for titles below CONTENT_TOP.
pub const TITLE_Y_OFFSET: u16 = 4;

/// Standard title Y position.
pub const TITLE_Y: u16 = CONTENT_TOP + TITLE_Y_OFFSET;

/// Full-width for content spanning most of the screen.
/// Used for headers and wide content areas.
pub const FULL_CONTENT_W: u16 = SCREEN_W - 2 * LARGE_MARGIN; // 448

/// Width for header/title regions (leaves room for status on right).
pub const HEADER_W: u16 = 300;

/// Width for status regions (battery, page number, etc.).
pub const STATUS_W: u16 = 144;

/// X position for right-aligned status in header.
pub const STATUS_X: u16 = SCREEN_W - LARGE_MARGIN - STATUS_W; // 320

// ── List/menu layout ────────────────────────────────────────────

/// Standard row height for list items.
pub const LIST_ROW_H: u16 = 52;

/// Gap between list rows.
pub const LIST_ROW_GAP: u16 = 4;

/// Combined row stride (row + gap).
pub const LIST_ROW_STRIDE: u16 = LIST_ROW_H + LIST_ROW_GAP;

/// Menu/settings row height (slightly smaller than list).
pub const MENU_ROW_H: u16 = 40;

/// Gap between menu rows.
pub const MENU_ROW_GAP: u16 = 6;

/// Combined menu row stride.
pub const MENU_ROW_STRIDE: u16 = MENU_ROW_H + MENU_ROW_GAP;

// ── Progress and overlays ───────────────────────────────────────

/// Height of progress bars.
pub const PROGRESS_H: u16 = 2;

/// Position overlay dimensions (for page/chapter display).
pub const POSITION_OVERLAY_W: u16 = 280;
pub const POSITION_OVERLAY_H: u16 = 40;

// ── Loading indicator ───────────────────────────────────────────

/// Default height for loading indicator region.
pub const LOADING_H: u16 = 24;

// ── Footer layout ───────────────────────────────────────────────

/// Standard footer height from bottom.
pub const FOOTER_MARGIN: u16 = 60;

/// Footer Y position.
pub const FOOTER_Y: u16 = SCREEN_H - FOOTER_MARGIN;
