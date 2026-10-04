// host stand-in for kernel/src/board/mod.rs (HAL): only the names the shared
// sources use. button/action/layout are the real files.
#[path = "../../../pulp/kernel/src/board/button.rs"]
pub mod button;
#[path = "../../../pulp/kernel/src/board/action.rs"]
pub mod action;
#[path = "../../../pulp/kernel/src/board/layout.rs"]
pub mod layout;

pub use crate::drivers::sdcard::SdStorage;
pub use crate::drivers::strip::StripBuffer;
pub use button::Button;

// SCREEN_W/H derive from the panel geometry exactly like kernel/src/board/mod.rs
// (X4) and kernel/src/board_c61/api.rs (C61): logical portrait 480 x 800.
pub const SCREEN_W: u16 = pulp_board_logic::ssd1677::HEIGHT; // 480
pub const SCREEN_H: u16 = pulp_board_logic::ssd1677::WIDTH; // 800

// display driver type named by AppLayer::run_special_mode; never constructed here
pub struct Epd;
