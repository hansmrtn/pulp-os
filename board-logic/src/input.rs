// Debounce / long-press / repeat state machine for single-key input (R12).
//
// Extracted from the X4 `kernel/src/drivers/input.rs` so that the OnePage C61
// and the X4 run the same code. The X4 driver is now a thin wrapper that
// supplies `RawSource` (ADC ladders + power button); its event stream is locked
// by `scripts/check-x4-input-trace.sh` (real input.rs compiled on the host and
// hashed against the pre-extraction output). The logic is deliberately a
// line-for-line move: same comparison operators (`>=`), same hold-timer
// restart when the raw reading deviates from the stable key, same event queue,
// same order of operations (raw is read only when the queue is empty, and the
// clock is read after the raw sample).
//
// Time is microseconds from any monotonic clock, so X4's `Instant` stays exact
// (no millisecond truncation).

/// Raw input must stay unchanged this long before it counts (ms). X4 value.
pub const DEBOUNCE_MS: u64 = 15;
/// Holding a key this long produces `LongPress` (ms). X4 value.
pub const LONG_PRESS_MS: u64 = 1000;
/// After a long press, `Repeat` is produced at this interval (ms). X4 value.
pub const REPEAT_MS: u64 = 150;
/// ADC samples averaged per reading.
pub const ADC_OVERSAMPLE: u32 = 4;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Event<K> {
    Press(K),
    Release(K),
    LongPress(K),
    Repeat(K),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct InputTiming {
    pub debounce_us: u64,
    pub long_press_us: u64,
    pub repeat_us: u64,
}

impl InputTiming {
    pub const fn from_ms(debounce_ms: u64, long_press_ms: u64, repeat_ms: u64) -> Self {
        Self {
            debounce_us: debounce_ms * 1000,
            long_press_us: long_press_ms * 1000,
            repeat_us: repeat_ms * 1000,
        }
    }

    /// The constants both boards use (the existing X4 behavior).
    pub const PULP: Self = Self::from_ms(DEBOUNCE_MS, LONG_PRESS_MS, REPEAT_MS);
}

/// Hardware side of one poll. `read_raw` is called first, then `now_us`
/// (X4 order); neither is called when an event is still queued.
pub trait RawSource<K> {
    /// The single key currently down, already prioritised by the board.
    fn read_raw(&mut self) -> Option<K>;
    /// Monotonic time in microseconds.
    fn now_us(&mut self) -> u64;
}

struct EventQueue<K> {
    buf: [Option<Event<K>>; 4],
}

impl<K: Copy> EventQueue<K> {
    const fn new() -> Self {
        Self { buf: [None; 4] }
    }

    fn push(&mut self, ev: Event<K>) {
        for slot in self.buf.iter_mut() {
            if slot.is_none() {
                *slot = Some(ev);
                return;
            }
        }
    }

    fn pop(&mut self) -> Option<Event<K>> {
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

pub struct InputCore<K> {
    timing: InputTiming,
    stable: Option<K>,
    candidate: Option<K>,
    candidate_since: u64,
    press_since: u64,
    long_press_fired: bool,
    last_repeat: u64,
    hold_consumed: bool,
    queue: EventQueue<K>,
}

impl<K: Copy + PartialEq> InputCore<K> {
    pub const fn new(timing: InputTiming, now_us: u64) -> Self {
        Self {
            timing,
            stable: None,
            candidate: None,
            candidate_since: now_us,
            press_since: now_us,
            long_press_fired: false,
            last_repeat: now_us,
            hold_consumed: false,
            queue: EventQueue::new(),
        }
    }

    /// Suppress `LongPress` / `Repeat` for the key currently held (until it is
    /// released). Used after a long press already consumed the hold.
    pub fn reset_hold_state(&mut self) {
        self.hold_consumed = true;
    }

    /// The debounced key currently down, if any.
    pub fn stable(&self) -> Option<K> {
        self.stable
    }

    /// Microseconds the stable key has been held at `now_us`.
    pub fn held_us(&self, now_us: u64) -> u64 {
        now_us.saturating_sub(self.press_since)
    }

    pub fn poll<S: RawSource<K>>(&mut self, src: &mut S) -> Option<Event<K>> {
        if !self.queue.is_empty() {
            return self.queue.pop();
        }

        let raw = src.read_raw();
        let now = src.now_us();

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

        let debounced = if now.saturating_sub(self.candidate_since) >= self.timing.debounce_us {
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

        if let Some(key) = self.stable
            && !self.hold_consumed
        {
            let held = now.saturating_sub(self.press_since);

            if !self.long_press_fired && held >= self.timing.long_press_us {
                self.long_press_fired = true;
                self.last_repeat = now;
                return Some(Event::LongPress(key));
            }

            if self.long_press_fired
                && now.saturating_sub(self.last_repeat) >= self.timing.repeat_us
            {
                self.last_repeat = now;
                return Some(Event::Repeat(key));
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    // Scripted source: the test sets `raw` and `t_us` before each poll.
    struct Src {
        raw: Option<u8>,
        t_us: u64,
        raw_reads: u32,
    }
    impl RawSource<u8> for Src {
        fn read_raw(&mut self) -> Option<u8> {
            self.raw_reads += 1;
            self.raw
        }
        fn now_us(&mut self) -> u64 {
            self.t_us
        }
    }

    const T0: u64 = 1_000_000; // clock does not start at 0
    const TIMING: InputTiming = InputTiming::PULP;

    struct Rig {
        core: InputCore<u8>,
        src: Src,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                core: InputCore::new(TIMING, T0),
                src: Src {
                    raw: None,
                    t_us: T0,
                    raw_reads: 0,
                },
            }
        }

        /// Poll once at `t_ms` milliseconds after T0 with `raw` held.
        fn poll_at(&mut self, t_ms: u64, raw: Option<u8>) -> Option<Event<u8>> {
            self.src.raw = raw;
            self.src.t_us = T0 + t_ms * 1000;
            self.core.poll(&mut self.src)
        }

        /// Poll every 1 ms over `[from_ms, to_ms]` inclusive; return (ms, event).
        fn scan(&mut self, from_ms: u64, to_ms: u64, raw: Option<u8>) -> Vec<(u64, Event<u8>)> {
            let mut out = Vec::new();
            for t in from_ms..=to_ms {
                if let Some(e) = self.poll_at(t, raw) {
                    out.push((t, e));
                }
            }
            out
        }
    }

    #[test]
    fn pulp_timing_constants_are_the_x4_values() {
        assert_eq!(DEBOUNCE_MS, 15);
        assert_eq!(LONG_PRESS_MS, 1000);
        assert_eq!(REPEAT_MS, 150);
        assert_eq!(ADC_OVERSAMPLE, 4);
        assert_eq!(
            InputTiming::PULP,
            InputTiming {
                debounce_us: 15_000,
                long_press_us: 1_000_000,
                repeat_us: 150_000
            }
        );
    }

    #[test]
    fn r12_debounce_press_needs_exactly_debounce_ms_of_stability() {
        // raw first differs at t=10 -> candidate_since = 10; Press at the poll
        // where now - candidate_since >= 15, i.e. t=25 and not t=24.
        let mut r = Rig::new();
        assert_eq!(r.poll_at(0, None), None);
        for t in 10..=24 {
            assert_eq!(r.poll_at(t, Some(1)), None, "t={t} is inside the window");
        }
        assert_eq!(r.poll_at(25, Some(1)), Some(Event::Press(1)));
        assert_eq!(r.core.stable(), Some(1));
    }

    #[test]
    fn r12_debounce_bounce_shorter_than_window_makes_no_event() {
        let mut r = Rig::new();
        // 14 ms pulses separated by 1 ms gaps never reach 15 ms stable
        for cycle in 0..20u64 {
            let base = cycle * 15;
            for t in 0..14 {
                assert_eq!(r.poll_at(base + t, Some(1)), None);
            }
            assert_eq!(r.poll_at(base + 14, None), None);
        }
        assert_eq!(r.core.stable(), None);
    }

    #[test]
    fn r12_debounce_restarts_on_every_change() {
        let mut r = Rig::new();
        // alternate every 10 ms (< 15 ms): never stable, no events, ever
        for i in 0..100u64 {
            let raw = if i % 2 == 0 { Some(1) } else { None };
            assert_eq!(r.poll_at(i * 10, raw), None);
        }
    }

    #[test]
    fn r12_debounce_release_is_debounced_and_reported_once() {
        let mut r = Rig::new();
        assert_eq!(r.scan(0, 30, Some(1)), vec![(15, Event::Press(1))]);
        // release glitch of 10 ms: raw back to 1 before debounce -> nothing
        for t in 31..=40 {
            assert_eq!(r.poll_at(t, None), None);
        }
        assert_eq!(r.scan(41, 70, Some(1)), vec![]);
        // real release: reported 15 ms after the raw change at t=71
        let ev = r.scan(71, 100, None);
        assert_eq!(ev, vec![(86, Event::Release(1))]);
    }

    #[test]
    fn r12_long_press_fires_exactly_at_threshold_from_the_press() {
        let mut r = Rig::new();
        let ev = r.scan(0, 1015, Some(1));
        // Press at 15; held = now - 15 >= 1000 -> LongPress at 1015, not 1014
        assert_eq!(ev, vec![(15, Event::Press(1)), (1015, Event::LongPress(1))]);
    }

    #[test]
    fn r12_repeat_interval_and_boundary() {
        let mut r = Rig::new();
        let ev = r.scan(0, 1015 + 450, Some(1));
        assert_eq!(
            ev,
            vec![
                (15, Event::Press(1)),
                (1015, Event::LongPress(1)),
                (1165, Event::Repeat(1)), // +150 exactly, not +149
                (1315, Event::Repeat(1)),
                (1465, Event::Repeat(1)),
            ]
        );
    }

    #[test]
    fn r12_release_stops_long_press_and_repeat() {
        let mut r = Rig::new();
        r.scan(0, 1400, Some(1));
        let ev = r.scan(1401, 3000, None);
        assert_eq!(ev, vec![(1416, Event::Release(1))]);
    }

    #[test]
    fn r12_reset_hold_state_suppresses_long_press_until_release() {
        let mut r = Rig::new();
        assert_eq!(r.scan(0, 100, Some(1)), vec![(15, Event::Press(1))]);
        r.core.reset_hold_state();
        assert_eq!(r.scan(101, 3000, Some(1)), vec![]);
        assert_eq!(r.scan(3001, 3020, None), vec![(3016, Event::Release(1))]);
        // the next press behaves normally again
        let ev = r.scan(3021, 4100, Some(1));
        assert_eq!(ev[0], (3036, Event::Press(1)));
        assert_eq!(ev[1], (4036, Event::LongPress(1)));
    }

    #[test]
    fn r12_sub_debounce_release_does_not_accumulate_into_long_press() {
        // a hold with a brief (<15 ms) gap restarts the hold timer: 600 ms
        // + gap + 600 ms must not LongPress.
        let mut r = Rig::new();
        r.scan(0, 600, Some(1));
        for t in 601..=608 {
            assert_eq!(r.poll_at(t, None), None);
        }
        let ev = r.scan(609, 1200, Some(1));
        assert_eq!(ev, vec![]);
        assert_eq!(r.core.stable(), Some(1));
    }

    #[test]
    fn r12_switching_keys_queues_release_then_press_without_reading_hardware() {
        let mut r = Rig::new();
        r.scan(0, 30, Some(1));
        // key 2 replaces key 1 at t=31: stable for 15 ms -> accepted at t=46
        for t in 31..=45 {
            assert_eq!(r.poll_at(t, Some(2)), None);
        }
        let reads_before = r.src.raw_reads;
        assert_eq!(r.poll_at(46, Some(2)), Some(Event::Release(1)));
        let reads_mid = r.src.raw_reads;
        assert_eq!(reads_mid, reads_before + 1);
        assert_eq!(r.poll_at(47, Some(2)), Some(Event::Press(2)));
        // the Press came from the queue: the source was not read again
        assert_eq!(r.src.raw_reads, reads_mid);
    }

    #[test]
    fn held_us_counts_from_the_press() {
        let mut r = Rig::new();
        r.scan(0, 100, Some(1));
        assert_eq!(r.core.held_us(T0 + 100_000), 85_000);
        assert_eq!(r.core.held_us(0), 0, "saturates instead of wrapping");
    }
}
