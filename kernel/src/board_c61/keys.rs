// OnePage C61 keys (R12, R13): esp-hal adapter only.
//
// Hardware (BSP board_keys.c:27-29, board_c61.c:63-66):
//   * front ladder: GPIO4 = ADC1_CH2, 11 dB attenuation (BSP ADC_ATTEN_DB_12),
//     curve-fitting calibration (BSP creates a curve-fitting cali handle), so
//     `read_oneshot` returns calibrated mV like the BSP's thresholds assume;
//   * side keys: GPIO2 (WAKE) / GPIO6 (PREV) / GPIO9 (NEXT), input with
//     internal pull-up, pressed = low.
// Decoding, 2.5 s startup grace, debounce, long-press, repeat and the
// key -> Action table are in pulp_board_logic::{keys, input} (host-tested).
//
// The ADC1 instance is created in `board_c61::adc` (T9), which enables GPIO4
// (this ladder) and GPIO5 (battery) in ONE `AdcConfig` and checks esp-hal's pin
// table (`ADC1_CH2 = GPIO4`, `ADC1_CH3 = GPIO5`) against the BSP's. This module
// only receives the already-enabled front pin and the shared handle (the BSP
// shares one adc_oneshot handle too, board_keys.c:119-121).
//
// Not verified on hardware: real ladder voltages vs the BSP windows, ADC
// calibration/attenuation behaviour, GPIO polarity and pull-ups, GPIO9 as a
// boot strapping pin, key feel.

use esp_hal::{
    gpio::{Input, InputConfig, Pull},
    peripherals::{GPIO2, GPIO6, GPIO9},
    time::Instant,
};
use pulp_board_logic::input::InputTiming;
use pulp_board_logic::keys::{AdcSample, Clock, KeyInput, KeyPin};

use super::adc::{FrontPin, SharedAdc};

pub use pulp_board_logic::input::Event;
pub use pulp_board_logic::keys::{Key, STARTUP_GRACE_US, map_event};

/// Front-ladder node: the shared ADC1 + GPIO4.
pub struct FrontLadder {
    adc: SharedAdc,
    pin: FrontPin,
}

impl AdcSample for FrontLadder {
    fn sample_mv(&mut self) -> Option<u16> {
        self.adc.read_mv(&mut self.pin)
    }
}

/// Side key: pull-up input, pressed = low.
pub struct GpioKey(Input<'static>);

impl KeyPin for GpioKey {
    fn is_low(&mut self) -> bool {
        self.0.is_low()
    }
}

pub struct HalClock;

impl Clock for HalClock {
    fn now_us(&mut self) -> u64 {
        Instant::now().duration_since_epoch().as_micros()
    }
}

pub type C61Input = KeyInput<FrontLadder, GpioKey, HalClock>;

fn key_pin(pin: impl esp_hal::gpio::InputPin + 'static) -> GpioKey {
    GpioKey(Input::new(pin, InputConfig::default().with_pull(Pull::Up)))
}

/// Create the key driver on the shared ADC1 (`adc::init`). The startup grace
/// window (2.5 s) starts now, like the BSP's `board_keys_init`.
pub fn new(
    adc: SharedAdc,
    front: FrontPin,
    key_wake: GPIO2<'static>,
    key_prev: GPIO6<'static>,
    key_next: GPIO9<'static>,
) -> C61Input {
    KeyInput::new(
        FrontLadder { adc, pin: front },
        [key_pin(key_wake), key_pin(key_prev), key_pin(key_next)],
        HalClock,
        InputTiming::PULP,
    )
}
