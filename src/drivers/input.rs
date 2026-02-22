//! Input event driver for XTEink X4
//!
//! The X4 has three physical input sources that all funnel into a
//! single "one button at a time" model:
//!
//! - **Row 1 ADC** (GPIO1): Right, Left, Confirm, Back via resistance ladder
//! - **Row 2 ADC** (GPIO2): Volume Up/Down via resistance ladder  
//! - **Power button** (GPIO3): Digital input, active low
//!
//! Because each resistance ladder can only report one press at a time,
//! we collapse everything into `Option<Button>` per poll cycle.

use esp_hal::time::{Duration, Instant};

use crate::board::button::{decode_ladder, Button, ROW1_THRESHOLDS, ROW2_THRESHOLDS};
use crate::board::InputHw;

/// Debounce time - ignore state changes shorter than this.
const DEBOUNCE_MS: u64 = 30;

/// Time held before firing a long-press event.
const LONG_PRESS_MS: u64 = 600;

/// Interval between repeat events when holding past long-press.
const REPEAT_MS: u64 = 150;

/// Input events returned from [`InputDriver::poll`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Button was just pressed.
    Press(Button),
    /// Button was just released.
    Release(Button),
    /// Button held long enough to trigger long-press.
    LongPress(Button),
    /// Button still held, firing repeat event.
    Repeat(Button),
}

/// Small fixed-size event queue for buffering multiple events per poll.
///
/// Needed because a single state change can produce both Release and Press.
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
        // If both slots are full, something is wrong with our logic.
        // Silently dropping is safer than panic in embedded.
    }

    fn pop(&mut self) -> Option<Event> {
        if (self.read as usize) < self.buf.len() {
            let idx = self.read as usize;
            if let Some(ev) = self.buf[idx].take() {
                self.read += 1;
                return Some(ev);
            }
        }
        // Reset for next cycle
        self.read = 0;
        None
    }

    fn is_empty(&self) -> bool {
        self.buf.iter().all(|s| s.is_none())
    }
}

/// Stateful input driver with debouncing, long-press, and repeat support.
pub struct InputDriver {
    hw: InputHw,
    /// Currently stable (debounced) button state.
    stable: Option<Button>,
    /// Candidate state during debounce window.
    candidate: Option<Button>,
    /// When the candidate state was first seen.
    candidate_since: Instant,
    /// When the current stable button was first pressed.
    press_since: Instant,
    /// Whether we've already fired a long-press for the current hold.
    long_press_fired: bool,
    /// When we last fired a repeat event.
    last_repeat: Instant,
    /// Buffered events to return.
    queue: EventQueue,
}

impl InputDriver {
    /// Create a new input driver from initialized hardware.
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
    ///
    /// Call this regularly (e.g., every 10-20ms). Returns `None` when
    /// there are no pending events.
    pub fn poll(&mut self) -> Option<Event> {
        // Drain any buffered events first
        if !self.queue.is_empty() {
            return self.queue.pop();
        }

        let raw = self.read_raw();
        let now = Instant::now();

        // Track candidate state for debouncing
        if raw != self.candidate {
            self.candidate = raw;
            self.candidate_since = now;
        }

        // Only accept the candidate as stable after debounce period
        let debounced = if now - self.candidate_since >= Duration::from_millis(DEBOUNCE_MS) {
            self.candidate
        } else {
            self.stable
        };

        // Handle state transitions
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

        // Handle held button: long-press and repeat
        if let Some(btn) = self.stable {
            let held = now - self.press_since;

            // Fire long-press once after threshold
            if !self.long_press_fired && held >= Duration::from_millis(LONG_PRESS_MS) {
                self.long_press_fired = true;
                self.last_repeat = now;
                return Some(Event::LongPress(btn));
            }

            // Fire repeat events at interval
            if self.long_press_fired && (now - self.last_repeat) >= Duration::from_millis(REPEAT_MS)
            {
                self.last_repeat = now;
                return Some(Event::Repeat(btn));
            }
        }

        None
    }

    /// Read raw button state from hardware (before debouncing).
    fn read_raw(&mut self) -> Option<Button> {
        // Power button has priority (digital, active low)
        if self.hw.power.is_low() {
            return Some(Button::Power);
        }

        // Read ADC channels
        let mv1: u16 = nb::block!(self.hw.adc.read_oneshot(&mut self.hw.row1)).unwrap();
        let mv2: u16 = nb::block!(self.hw.adc.read_oneshot(&mut self.hw.row2)).unwrap();

        // Decode resistance ladder readings
        decode_ladder(mv1, ROW1_THRESHOLDS).or_else(|| decode_ladder(mv2, ROW2_THRESHOLDS))
    }
}
