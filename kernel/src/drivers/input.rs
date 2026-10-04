// debounced input from ADC ladders and power button
// one button at a time (ladder hw limitation)
// ADC reads oversampled to reject noise (~40 us per channel)
//
// The debounce / long-press / repeat state machine is
// pulp_board_logic::input::InputCore (shared with the OnePage C61); this file
// is only the X4 hardware side: two ADC ladders + the power button, in the
// same priority order as before (power, row 1, row 2). The event stream is
// locked by scripts/check-x4-input-trace.sh.

use esp_hal::time::Instant;
use pulp_board_logic::input::{InputCore, InputTiming, RawSource};

use crate::board::InputHw;
use crate::board::button::{Button, ROW1_THRESHOLDS, ROW2_THRESHOLDS, decode_ladder};
use crate::kernel::timing;

macro_rules! read_averaged {
    ($adc:expr, $pin:expr) => {{
        let mut sum: u32 = 0;
        for _ in 0..timing::ADC_OVERSAMPLE {
            sum += nb::block!($adc.read_oneshot($pin)).unwrap() as u32;
        }
        (sum / timing::ADC_OVERSAMPLE) as u16
    }};
}

pub type Event = pulp_board_logic::input::Event<Button>;

fn now_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

struct X4Source {
    hw: InputHw,
    last_now_us: u64,
}

impl RawSource<Button> for X4Source {
    fn read_raw(&mut self) -> Option<Button> {
        let power_low = crate::board::power_button_is_low();
        if power_low {
            return Some(Button::Power);
        }

        let mv1 = read_averaged!(self.hw.adc, &mut self.hw.row1);
        let mv2 = read_averaged!(self.hw.adc, &mut self.hw.row2);

        decode_ladder(mv1, ROW1_THRESHOLDS).or_else(|| decode_ladder(mv2, ROW2_THRESHOLDS))
    }

    fn now_us(&mut self) -> u64 {
        self.last_now_us = now_us();
        self.last_now_us
    }
}

pub struct InputDriver {
    src: X4Source,
    core: InputCore<Button>,
}

impl InputDriver {
    pub fn new(hw: InputHw) -> Self {
        let now = now_us();
        Self {
            src: X4Source {
                hw,
                last_now_us: now,
            },
            core: InputCore::new(
                InputTiming::from_ms(
                    timing::DEBOUNCE_MS,
                    timing::LONG_PRESS_MS,
                    timing::REPEAT_MS,
                ),
                now,
            ),
        }
    }

    pub fn reset_hold_state(&mut self) {
        self.core.reset_hold_state();
    }

    pub fn poll(&mut self) -> Option<Event> {
        let ev = self.core.poll(&mut self.src);
        if let Some(Event::LongPress(btn)) = ev {
            log::info!(
                "input: LongPress({:?}) after {}ms",
                btn,
                self.core.held_us(self.src.last_now_us) / 1000
            );
        }
        ev
    }

    pub fn read_battery_mv(&mut self) -> u16 {
        read_averaged!(self.src.hw.adc, &mut self.src.hw.battery)
    }
}
