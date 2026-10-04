// board support for the OnePage C61 (ESP32-C61)
//
// T4: pin ownership and the GPIO27 boot/runtime power contract.
// T5: SPI2/DMA bus with arbitrated EPD/SD devices (`spi`), SD bring-up, card
// detect and storage-error mapping (`sd`).
// T6: SSD1677 full refresh (`epd`): esp-hal adapter over pulp_board_logic::ssd1677.
// T7: keys (`keys`): ADC ladder + 3 GPIO keys over pulp_board_logic::{keys, input}.
// T8: memory placement (`memory`): PSRAM bring-up on a private heap, budget-checked
// allocation, internal-only DMA buffers (policy in pulp_board_logic::memory).
// T9: shared ADC1 (`adc`: keys GPIO4 + battery GPIO5 in one AdcConfig), battery
// sampling with the charge-pause contract (`battery`), USB detect (`usb`).
// T10: session persistence on SD (`session`): SessionStore adapter + save/restore
// over pulp_board_logic::session (two slot files, CRC, SD-active gate).
// T11: deep sleep (`sleep`): wake GPIO2, EPD park, line silencing and the
// esp-hal sleep entry over pulp_board_logic::sleep (order + failure policy).
// T12: `api` (the `pulp_kernel::board` surface the kernel/apps compile against)
// and `hw` (what the kernel owns on this board: power rail, battery, sleep parts,
// card detect).

pub mod adc;
pub mod api;
pub mod battery;
pub mod epd;
pub mod hw;
pub mod keys;
pub mod memory;
pub mod pins;
pub mod power;
pub mod sd;
pub mod session;
pub mod sleep;
pub mod spi;
pub mod usb;

pub use pins::Pins;
pub use pulp_board_logic::power::{
    DisplayReset, PeripheralPower, PowerError, RailState, SdInitPermit,
};
