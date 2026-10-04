// OnePage C61 battery (R16): esp-hal adapter over pulp_board_logic::battery.
//
// The pause -> settle -> sample -> resume sequence, the failure handling and
// the voltage/percent maths are host-tested in the shared crate; this file maps
// the traits onto hardware:
//   * GPIO10 push-pull output, created HIGH (charging allowed; BSP
//     board_c61.c:268-270), owned by the monitor (no other writer exists);
//   * GPIO5 = ADC1_CH3 on the shared ADC1 (`board_c61::adc`);
//   * delay = blocking busy-wait (`power::HalDelay`): one measurement blocks
//     the caller for ~30 ms settle + 16 conversions. T12 decides which task
//     calls it and how often (X4 reads every 30 s).
//
// Not verified on hardware: divider ratio (schematic says 1:1, 5.1 MOhm each),
// ADC accuracy with a ~2.55 MOhm source, whether 30 ms is enough for the pin to
// settle after the charger is paused, charger behaviour on repeated pauses.

use esp_hal::{
    gpio::{Level, Output, OutputConfig},
    peripherals::GPIO10,
};
use pulp_board_logic::battery::{BatteryMonitor, ChargePin};
use pulp_board_logic::keys::AdcSample;

pub use pulp_board_logic::battery::{BatteryError, BatteryReading};

use super::adc::{BatteryPin, SharedAdc};
use super::power::HalDelay;

/// GPIO10 charge enable (high = charging allowed, low = paused).
pub struct Gpio10Charge(Output<'static>);

impl ChargePin for Gpio10Charge {
    fn set_charging(&mut self, enabled: bool) {
        self.0
            .set_level(if enabled { Level::High } else { Level::Low });
    }
}

/// Battery node: the shared ADC1 + GPIO5.
pub struct BatteryAdc {
    adc: SharedAdc,
    pin: BatteryPin,
}

impl AdcSample for BatteryAdc {
    fn sample_mv(&mut self) -> Option<u16> {
        self.adc.read_mv(&mut self.pin)
    }
}

pub type C61Battery = BatteryMonitor<Gpio10Charge, BatteryAdc, HalDelay>;

/// Build the battery monitor. GPIO10 starts HIGH (no pause glitch at boot).
pub fn new(gpio10: GPIO10<'static>, adc: SharedAdc, pin: BatteryPin) -> C61Battery {
    let charge = Gpio10Charge(Output::new(gpio10, Level::High, OutputConfig::default()));
    BatteryMonitor::new(charge, BatteryAdc { adc, pin }, HalDelay::new())
}
