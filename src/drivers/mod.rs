// Hardware drivers: chip-level and protocol-level, board-independent.
// Each module is reusable across boards; pin assignments and bus wiring in board/.

pub mod battery;
pub mod input;
pub mod sdcard;
pub mod ssd1677;
pub mod storage;
pub mod strip;
