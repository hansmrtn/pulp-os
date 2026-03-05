// shared layout constants for UI rendering
//
// only constants used by 2+ apps belong here.
// single-use layout values should be defined locally.

use super::statusbar::BAR_HEIGHT;
use crate::board::SCREEN_W;

pub const CONTENT_TOP: u16 = BAR_HEIGHT;
pub const LARGE_MARGIN: u16 = 16;
pub const SECTION_GAP: u16 = 8;
pub const TITLE_Y_OFFSET: u16 = 4;
pub const TITLE_Y: u16 = CONTENT_TOP + TITLE_Y_OFFSET;
pub const FULL_CONTENT_W: u16 = SCREEN_W - 2 * LARGE_MARGIN;
pub const HEADER_W: u16 = 300;
