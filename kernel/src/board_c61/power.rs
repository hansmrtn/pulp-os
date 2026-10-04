// esp-hal adapters for the HAL-free GPIO27 state machine
// (pulp_board_logic::power). Keep this file thin: the ordering rules live in
// the host-tested crate, this only maps traits onto esp-hal.

use esp_hal::{
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    peripherals::GPIO27,
};
use pulp_board_logic::power::{DelayMs, PeripheralPower, RailPin};

/// GPIO27 as push-pull output. Private to this module's constructor path:
/// the only holder is `PeripheralPower`, so no other code can toggle it.
pub struct Gpio27Rail(Output<'static>);

impl RailPin for Gpio27Rail {
    fn set_high(&mut self) {
        self.0.set_high();
    }
    fn set_low(&mut self) {
        self.0.set_low();
    }
}

/// Blocking busy-wait delay: the boot power-cycle (2 x 20 ms) and the battery
/// charge-pause settle (30 ms, T9).
pub struct HalDelay(Delay);

impl HalDelay {
    pub fn new() -> Self {
        Self(Delay::new())
    }
}

impl Default for HalDelay {
    fn default() -> Self {
        Self::new()
    }
}

impl DelayMs for HalDelay {
    fn delay_ms(&mut self, ms: u32) {
        self.0.delay_millis(ms);
    }
}

/// Take GPIO27 as the peripheral-power rail. Starts low (BSP configures it as
/// output and drives low first, board_c61.c:74-81), state `Unpowered`.
pub fn take_rail(gpio27: GPIO27<'static>) -> PeripheralPower<Gpio27Rail> {
    let out = Output::new(gpio27, Level::Low, OutputConfig::default());
    PeripheralPower::new(Gpio27Rail(out))
}
