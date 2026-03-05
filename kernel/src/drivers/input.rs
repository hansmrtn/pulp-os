// debounced input from ADC ladders and power button
// one button at a time (ladder hw limitation)
// ADC reads oversampled to reject noise (~40 us per channel)

use esp_hal::time::{Duration, Instant};

use crate::board::button::{decode_ladder, Button, ROW1_THRESHOLDS, ROW2_THRESHOLDS};
use crate::board::InputHw;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Press(Button),
    Release(Button),
    LongPress(Button),
    Repeat(Button),
}

struct EventQueue {
    buf: [Option<Event>; 4],
}

impl EventQueue {
    const fn new() -> Self {
        Self { buf: [None; 4] }
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
    hold_consumed: bool,
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
            hold_consumed: false,
            queue: EventQueue::new(),
        }
    }

    pub fn reset_hold_state(&mut self) {
        self.hold_consumed = true;
    }

    pub fn poll(&mut self) -> Option<Event> {
        if !self.queue.is_empty() {
            return self.queue.pop();
        }

        let raw = self.read_raw();
        let now = Instant::now();

        if raw != self.candidate {
            // raw deviated from stable; restart hold timer so
            // sub-debounce releases don't accumulate into LongPress
            if self.stable.is_some() && raw != self.stable {
                self.press_since = now;
                self.long_press_fired = false;
                self.last_repeat = now;
            }
            self.candidate = raw;
            self.candidate_since = now;
        }

        let debounced = if now - self.candidate_since >= Duration::from_millis(timing::DEBOUNCE_MS)
        {
            self.candidate
        } else {
            self.stable
        };

        if debounced != self.stable {
            if let Some(old) = self.stable {
                self.queue.push(Event::Release(old));
                self.hold_consumed = false;
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
            if !self.hold_consumed {
                let held = now - self.press_since;

                if !self.long_press_fired && held >= Duration::from_millis(timing::LONG_PRESS_MS) {
                    self.long_press_fired = true;
                    self.last_repeat = now;
                    return Some(Event::LongPress(btn));
                }

                if self.long_press_fired
                    && (now - self.last_repeat) >= Duration::from_millis(timing::REPEAT_MS)
                {
                    self.last_repeat = now;
                    return Some(Event::Repeat(btn));
                }
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
        read_averaged!(self.hw.adc, &mut self.hw.row1)
    }

    fn read_averaged_row2(&mut self) -> u16 {
        read_averaged!(self.hw.adc, &mut self.hw.row2)
    }

    pub fn read_battery_mv(&mut self) -> u16 {
        read_averaged!(self.hw.adc, &mut self.hw.battery)
    }
}
