// hardware drivers: chip-level and protocol-level, board-independent

// battery/input/ssd1677 are written against the X4 (esp32c3) HAL surface. The
// C61 equivalents are board_c61::{battery, keys, epd} (T6, T7, T9); the pure
// maths and state machines are shared through pulp_board_logic. Only the thin
// name-compat modules below exist on the C61, so the scheduler, tasks and apps
// can use `drivers::input::Event` / `drivers::battery::*` on both boards.
#[cfg(feature = "board-x4")]
pub mod battery;
#[cfg(feature = "board-x4")]
pub mod input;
pub mod sdcard;
#[cfg(feature = "board-x4")]
pub mod ssd1677;
pub mod storage;
// T12: StripBuffer (DrawTarget over the shared StripCore) is board-neutral
pub mod strip;

// OnePage C61: hardware event = the C61 key event type (no Power button)
#[cfg(feature = "board-onepage-c61")]
pub mod input {
    pub type Event = pulp_board_logic::input::Event<crate::board::button::Button>;
}

// OnePage C61: the monitor reports the cell voltage already (x2 divider is
// applied in pulp_board_logic::battery), so only the percentage is needed here
#[cfg(feature = "board-onepage-c61")]
pub mod battery {
    pub fn battery_percentage(cell_mv: u16) -> u8 {
        pulp_board_logic::battery::percentage(cell_mv)
    }
}
