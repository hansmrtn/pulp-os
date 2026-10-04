// OnePage C61 pin ownership. One esp-hal GPIO singleton per field, so the
// type system guarantees a pin has exactly one owner. GPIO27 is not exposed
// as a raw pin: it is wrapped by `PeripheralPower` (see `power`), because it
// is both EPD RST and the SD/MIC power enable. Numbers and BSP line references:
// pulp_board_logic::pins (the HAL-free mirror, checked by host tests).

use esp_hal::peripherals::{
    GPIO2, GPIO3, GPIO4, GPIO5, GPIO6, GPIO7, GPIO8, GPIO9, GPIO10, GPIO11, GPIO22, GPIO23, GPIO24,
    GPIO25, GPIO26, GPIO28, GPIO29,
};
use pulp_board_logic::power::PeripheralPower;

use super::power::Gpio27Rail;

/// Raw pins, not yet configured. Later tasks turn them into SPI / Input /
/// Output / ADC drivers (they take the field by value).
pub struct Pins {
    // shared SPI2 bus (T5)
    pub spi_sck: GPIO22<'static>,
    pub spi_mosi: GPIO23<'static>,
    pub spi_miso: GPIO24<'static>,
    // EPD (T6); RST is GPIO27 and lives in `power`
    pub epd_cs: GPIO25<'static>,
    pub epd_dc: GPIO8<'static>,
    pub epd_busy: GPIO29<'static>,
    // SD (T5)
    pub sd_cs: GPIO26<'static>,
    pub sd_card_detect: GPIO28<'static>,
    // keys (T7): front ADC ladder + 3 side keys; wake is LP-capable (T11)
    pub front_adc: GPIO4<'static>,
    pub key_wake: GPIO2<'static>,
    pub key_prev: GPIO6<'static>,
    pub key_next: GPIO9<'static>,
    // battery / USB (T9)
    pub battery_adc: GPIO5<'static>,
    pub charge_enable: GPIO10<'static>,
    pub usb_detect: GPIO11<'static>,
    // reserved PDM mic pins; CLK is silenced before the rail is cut (T11)
    pub mic_pdm_clk: GPIO7<'static>,
    pub mic_pdm_data: GPIO3<'static>,
    /// GPIO27: SD/MIC power enable and EPD RST. Sole owner of the pin.
    pub power: PeripheralPower<Gpio27Rail>,
}

/// Move every board pin out of `esp_hal::peripherals::Peripherals`.
/// A macro (not a fn) because it partially moves fields, leaving the other
/// peripherals (TIMG0, SPI2, ...) usable by the caller.
#[macro_export]
macro_rules! take_c61_pins {
    ($p:ident) => {{
        $crate::board_c61::Pins {
            spi_sck: $p.GPIO22,
            spi_mosi: $p.GPIO23,
            spi_miso: $p.GPIO24,
            epd_cs: $p.GPIO25,
            epd_dc: $p.GPIO8,
            epd_busy: $p.GPIO29,
            sd_cs: $p.GPIO26,
            sd_card_detect: $p.GPIO28,
            front_adc: $p.GPIO4,
            key_wake: $p.GPIO2,
            key_prev: $p.GPIO6,
            key_next: $p.GPIO9,
            battery_adc: $p.GPIO5,
            charge_enable: $p.GPIO10,
            usb_detect: $p.GPIO11,
            mic_pdm_clk: $p.GPIO7,
            mic_pdm_data: $p.GPIO3,
            power: $crate::board_c61::power::take_rail($p.GPIO27),
        }
    }};
}
