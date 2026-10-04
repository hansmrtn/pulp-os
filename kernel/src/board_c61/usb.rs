// OnePage C61 USB detect (R17): esp-hal adapter over pulp_board_logic::usb.
//
// GPIO11 as input with the internal pull-up (BSP board_c61.c:274-279; the board
// also has an external 10 kOhm pull-up, R29). The polarity is NOT decided here:
// it is `pulp_board_logic::usb::USB_POLARITY`, with the evidence and the
// BSP-README contradiction documented next to it. Unconfirmed on hardware.

use esp_hal::{
    gpio::{Input, InputConfig, Pull},
    peripherals::GPIO11,
};
use pulp_board_logic::usb::UsbDetect;

pub use pulp_board_logic::usb::{USB_DEBOUNCE_SAMPLES, USB_POLARITY, UsbEvent, UsbPolarity};

/// GPIO11 plus the debounced plugged/unplugged state.
pub struct UsbPort {
    pin: Input<'static>,
    detect: UsbDetect,
}

impl UsbPort {
    /// Configure GPIO11 and take the first sample as the initial state (no
    /// event for it).
    pub fn new(gpio11: GPIO11<'static>) -> Self {
        let pin = Input::new(gpio11, InputConfig::default().with_pull(Pull::Up));
        let detect = UsbDetect::for_board(pin.is_high());
        Self { pin, detect }
    }

    /// Raw GPIO11 level (for diagnostics / polarity bring-up).
    pub fn level_high(&self) -> bool {
        self.pin.is_high()
    }

    pub fn plugged(&self) -> bool {
        self.detect.plugged()
    }

    /// Sample the pin once; returns an event only when the debounced state
    /// changes. Call at a fixed period (the debounce is N consecutive samples).
    pub fn poll(&mut self) -> Option<UsbEvent> {
        self.detect.sample(self.pin.is_high())
    }
}
