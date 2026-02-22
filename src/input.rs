//! Input event driver
//!
//! The XTEink X4 has three physical input sources that all funnel
//! into a single "one button at a time" model:
//! Because each resistance ladder can only report one press at a
//! time, and the power button is digital, we collapse everything
//! into `Option<Button>` per poll cycle.
//!

use esp_hal::time::{Duration, Instant};

use crate::board::{self, Button, InputHw, ROW1_THRESHOLDS, ROW2_THRESHOLDS};

const DEBOUNCE_MS: u64 = 30;
const LONG_PRESS_MS: u64 = 600;
const REPEAT_MS: u64 = 150;

/// Events are returned one at a time from InputDriver::Poll
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Press(Button),
    Release(Button),
    LongPress(Button),
    Repeat(Button),
}

struct EventQueue {
    buf: [Option<Event>; 2],
    read: u8,
}

impl EventQueue {
    const fn new() -> Self {
        Self {
            buf: [None; 2],
            read: 0,
        }
    }

    fn push(&mut self, ev: Event) {
        for slot in self.buf.iter_mut() {
            if slot.is_none() {
                *slot = Some(ev);
                return;
            }
        }
        // NOTE: If both slots are full something is wrong with our logic,
        // silently dropping is safer than panic.
    }

    fn pop(&mut self) -> Option<Event> {
        if (self.read as usize) < self.buf.len() {
            let idx = self.read as usize;
            if let Some(ev) = self.buf[idx].take() {
                self.read += 1;
                return Some(ev);
            }
        }
        // Reset for next cycle.
        self.read = 0;
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

    /// Poll for the next input event.
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
        if self.hw.power.is_low() {
            return Some(Button::Power);
        }

        let mv1: u16 = nb::block!(self.hw.adc.read_oneshot(&mut self.hw.row1)).unwrap();
        let mv2: u16 = nb::block!(self.hw.adc.read_oneshot(&mut self.hw.row2)).unwrap();

        board::decode_ladder(mv1, ROW1_THRESHOLDS)
            .or_else(|| board::decode_ladder(mv2, ROW2_THRESHOLDS))
    }
}
