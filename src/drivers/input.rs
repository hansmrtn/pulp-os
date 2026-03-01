// Debounced input from ADC ladders and power button.
// One button at a time (ladder hw limitation). Sources: Row1 (GPIO1),
// Row2 (GPIO2), Power (GPIO3 interrupt). 15ms debounce, 1s long press, 150ms repeat.
// ADC reads oversampled (4 samples averaged) to reject noise from ESP32-C3
// non-linearity and battery sag during SPI traffic; ~40us per channel.

use esp_hal::time::{Duration, Instant};

use crate::board::InputHw;
use crate::board::button::{Button, ROW1_THRESHOLDS, ROW2_THRESHOLDS, decode_ladder};

const DEBOUNCE_MS: u64 = 15;
const LONG_PRESS_MS: u64 = 1000;
const REPEAT_MS: u64 = 150;
const ADC_OVERSAMPLE: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Press(Button),
    Release(Button),
    LongPress(Button),
    Repeat(Button),
}

// max 2: one debounce cycle pushes at most Release(old) + Press(new)
struct EventQueue {
    buf: [Option<Event>; 2],
}

impl EventQueue {
    const fn new() -> Self {
        Self { buf: [None; 2] }
    }

    fn push(&mut self, ev: Event) {
        for slot in self.buf.iter_mut() {
            if slot.is_none() {
                *slot = Some(ev);
                return;
            }
        }
    }

    fn pop(&mut self) -> Option<Event> {
        for slot in self.buf.iter_mut() {
            if let Some(ev) = slot.take() {
                return Some(ev);
            }
        }
        None
    }

    fn is_empty(&self) -> bool {
        self.buf.iter().all(|s| s.is_none())
    }
}

pub struct InputDriver {
    hw: InputHw,
    stable: Option<Button>,
    candidate: Option<Button>,
    candidate_since: Instant,
    press_since: Instant,
    long_press_fired: bool,
    last_repeat: Instant,
    queue: EventQueue,
}

impl InputDriver {
    pub fn new(hw: InputHw) -> Self {
        let now = Instant::now();
        Self {
            hw,
            stable: None,
            candidate: None,
            candidate_since: now,
            press_since: now,
            long_press_fired: false,
            last_repeat: now,
            queue: EventQueue::new(),
        }
    }

    pub fn poll(&mut self) -> Option<Event> {
        if !self.queue.is_empty() {
            return self.queue.pop();
        }

        let raw = self.read_raw();
        let now = Instant::now();

        if raw != self.candidate {
            self.candidate = raw;
            self.candidate_since = now;
        }

        let debounced = if now - self.candidate_since >= Duration::from_millis(DEBOUNCE_MS) {
            self.candidate
        } else {
            self.stable
        };

        if debounced != self.stable {
            if let Some(old) = self.stable {
                self.queue.push(Event::Release(old));
            }
            if let Some(new) = debounced {
                self.queue.push(Event::Press(new));
                self.press_since = now;
                self.long_press_fired = false;
                self.last_repeat = now;
            }
            self.stable = debounced;
            return self.queue.pop();
        }

        if let Some(btn) = self.stable {
            let held = now - self.press_since;

            if !self.long_press_fired && held >= Duration::from_millis(LONG_PRESS_MS) {
                self.long_press_fired = true;
                self.last_repeat = now;
                return Some(Event::LongPress(btn));
            }

            if self.long_press_fired && (now - self.last_repeat) >= Duration::from_millis(REPEAT_MS)
            {
                self.last_repeat = now;
                return Some(Event::Repeat(btn));
            }
        }

        None
    }

    fn read_raw(&mut self) -> Option<Button> {
        if crate::board::power_button_is_low() {
            return Some(Button::Power);
        }

        let mv1 = self.read_averaged_row1();
        let mv2 = self.read_averaged_row2();

        decode_ladder(mv1, ROW1_THRESHOLDS).or_else(|| decode_ladder(mv2, ROW2_THRESHOLDS))
    }

    fn read_averaged_row1(&mut self) -> u16 {
        let mut sum: u32 = 0;
        for _ in 0..ADC_OVERSAMPLE {
            sum += nb::block!(self.hw.adc.read_oneshot(&mut self.hw.row1)).unwrap() as u32;
        }
        (sum / ADC_OVERSAMPLE) as u16
    }

    fn read_averaged_row2(&mut self) -> u16 {
        let mut sum: u32 = 0;
        for _ in 0..ADC_OVERSAMPLE {
            sum += nb::block!(self.hw.adc.read_oneshot(&mut self.hw.row2)).unwrap() as u32;
        }
        (sum / ADC_OVERSAMPLE) as u16
    }

    pub fn read_battery_mv(&mut self) -> u16 {
        let mut sum: u32 = 0;
        for _ in 0..ADC_OVERSAMPLE {
            sum += nb::block!(self.hw.adc.read_oneshot(&mut self.hw.battery)).unwrap() as u32;
        }
        (sum / ADC_OVERSAMPLE) as u16
    }
}
