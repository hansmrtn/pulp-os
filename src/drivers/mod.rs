// Hardware drivers — chip-level and protocol-level, board-independent.
//
// Each module is reusable across boards; only pin assignments and bus
// wiring (in board/) are board-specific.

pub mod battery;
pub mod input;
pub mod sdcard;
pub mod ssd1677;
pub mod storage;
pub mod strip;
