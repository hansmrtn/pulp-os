// host stand-in for kernel/src/drivers/mod.rs: strip is the real file; sdcard and
// storage are the in-memory SD shim; input only provides the event type.
pub mod sdcard;
pub mod storage;

#[path = "../../../pulp/kernel/src/drivers/strip.rs"]
pub mod strip;

// the pre-port strip.rs imports panel constants from the SSD1677 driver
pub mod ssd1677 {
    pub use pulp_board_logic::ssd1677::{HEIGHT, Rotation, WIDTH};
}

pub mod input {
    // X4 hardware event (kernel/src/drivers/input.rs: `Event = input::Event<Button>`)
    pub type Event = pulp_board_logic::input::Event<crate::board::button::Button>;
}
