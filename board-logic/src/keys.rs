// OnePage C61 keys: 1 front ADC ladder (GPIO4, ADC1) + 3 side GPIO keys
// (R12, R13). Decoding, startup grace, and the key -> `Action` table are pure
// logic here; hardware is reached through `AdcSample`, `KeyPin`, `Clock`.
//
// BSP facts (../bsp_onepage_c61):
//   * board_keys.c:27-29   side keys GPIO2 (WAKE) / GPIO6 (PREV) / GPIO9 (NEXT),
//                          front ladder GPIO4 = ADC1_CH2 (see FRONT_ADC_CHANNEL)
//   * board_keys.c:32-44   ladder windows in mV, measured on the board (rest
//                          ~3100): BACK 2400-2800 (~2592), LEFT 1780-2140
//                          (~1956), RIGHT 1140-1500 (~1316), ENTER 0-250 (~0);
//                          tight windows with dead zones on purpose
//   * board_keys.c:104 GPIO keys: active level 0 (pressed = grounded),
//                          internal pull-up
//   * board_keys.c:19-22,57 startup grace 2.5 s: ALL key events are dropped
//                          (the callback is shared by GPIO and ADC keys)
//   * board_keys_moui.c:21-29 key -> reader semantics: WAKE->BACK, PREV->UP,
//                          NEXT->DOWN, BACK->BACK, LEFT->LEFT, RIGHT->RIGHT,
//                          ENTER->ENTER
//
// Everything marked "unverified" in the docs is a BSP-derived value that has
// not been checked on hardware.

use crate::action::{Action, ActionEvent};
use crate::input::{ADC_OVERSAMPLE, Event, InputCore, InputTiming, RawSource};

/// Startup grace in microseconds (BSP KEYS_STARTUP_GRACE_US, board_keys.c:22).
pub const STARTUP_GRACE_US: u64 = 2_500_000;
/// Front ladder ADC channel (ADC1_CH2 = GPIO4). Checked against esp-hal's own
/// pin table when the kernel adapter is created.
pub const FRONT_ADC_CHANNEL: u8 = 2;
/// GPIO keys are active-low (pressed = pin grounded), pull-up enabled.
pub const GPIO_KEYS_ACTIVE_LOW: bool = true;

/// Physical keys, in BSP `onepage_key_t` order (board.h:26-34).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Side key GPIO2 (also the deep-sleep wake source, T11).
    Wake,
    /// Side key GPIO6.
    Prev,
    /// Side key GPIO9 (boot strapping pin).
    Next,
    /// Front ladder.
    Back,
    Left,
    Right,
    Enter,
}

impl Key {
    pub const ALL: [Key; 7] = [
        Key::Wake,
        Key::Prev,
        Key::Next,
        Key::Back,
        Key::Left,
        Key::Right,
        Key::Enter,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Key::Wake => "WAKE",
            Key::Prev => "PREV",
            Key::Next => "NEXT",
            Key::Back => "BACK",
            Key::Left => "LEFT",
            Key::Right => "RIGHT",
            Key::Enter => "ENTER",
        }
    }

    /// Existing semantic action for this key (no new `Action`s).
    ///
    /// Default table, from the BSP moui map (board_keys_moui.c:21-29) read
    /// through the X4 `ButtonMapper` (UP/prev -> `Prev`, DOWN/next -> `Next`,
    /// LEFT/RIGHT -> `PrevJump`/`NextJump`, ENTER -> `Select`):
    ///
    /// | key   | action     |
    /// |-------|------------|
    /// | WAKE  | Back       |
    /// | PREV  | Prev       |
    /// | NEXT  | Next       |
    /// | BACK  | Back       |
    /// | LEFT  | PrevJump   |
    /// | RIGHT | NextJump   |
    /// | ENTER | Select     |
    ///
    /// `swap` is the X4 left-handed layout (Back<->Left, Enter<->Right on the
    /// front row). Side keys and WAKE never swap, like the X4 volume/power keys.
    /// `Action::Menu` has no key on the C61 (X4 uses Power for it).
    pub const fn action(self, swap: bool) -> Action {
        match (self, swap) {
            (Key::Wake, _) => Action::Back,
            (Key::Prev, _) => Action::Prev,
            (Key::Next, _) => Action::Next,
            (Key::Back, false) => Action::Back,
            (Key::Back, true) => Action::PrevJump,
            (Key::Left, false) => Action::PrevJump,
            (Key::Left, true) => Action::Back,
            (Key::Right, false) => Action::NextJump,
            (Key::Right, true) => Action::Select,
            (Key::Enter, false) => Action::Select,
            (Key::Enter, true) => Action::NextJump,
        }
    }
}

pub fn map_event(ev: Event<Key>, swap: bool) -> ActionEvent {
    match ev {
        Event::Press(k) => ActionEvent::Press(k.action(swap)),
        Event::Release(k) => ActionEvent::Release(k.action(swap)),
        Event::LongPress(k) => ActionEvent::LongPress(k.action(swap)),
        Event::Repeat(k) => ActionEvent::Repeat(k.action(swap)),
    }
}

/// One ladder key: pressed while `min_mv <= mv <= max_mv` (calibrated mV).
/// Whether the BSP's button_adc includes `max` is unverified (its source is not
/// local); it changes the answer for exactly one mV at each upper edge.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LadderWindow {
    pub key: Key,
    pub min_mv: u16,
    pub max_mv: u16,
}

/// board_keys.c:32-44, highest voltage first.
pub const FRONT_LADDER: [LadderWindow; 4] = [
    LadderWindow {
        key: Key::Back,
        min_mv: 2400,
        max_mv: 2800,
    },
    LadderWindow {
        key: Key::Left,
        min_mv: 1780,
        max_mv: 2140,
    },
    LadderWindow {
        key: Key::Right,
        min_mv: 1140,
        max_mv: 1500,
    },
    LadderWindow {
        key: Key::Enter,
        min_mv: 0,
        max_mv: 250,
    },
];

/// Decode one calibrated front-ladder reading. Rest (~3100 mV), saturation and
/// the dead zones between windows decode to `None`.
pub fn decode_front_ladder(mv: u16) -> Option<Key> {
    for w in FRONT_LADDER {
        if mv >= w.min_mv && mv <= w.max_mv {
            return Some(w.key);
        }
    }
    None
}

/// GPIO keys; several down at once resolve by fixed priority WAKE > PREV > NEXT
/// (the input path is single-key, like the X4 where Power wins over the ladder).
pub fn decode_gpio(wake_low: bool, prev_low: bool, next_low: bool) -> Option<Key> {
    if wake_low {
        Some(Key::Wake)
    } else if prev_low {
        Some(Key::Prev)
    } else if next_low {
        Some(Key::Next)
    } else {
        None
    }
}

// ---- hardware traits ------------------------------------------------------

/// One calibrated ADC reading of a node (front ladder, battery), in mV. `None`
/// = the conversion failed; it must never be reported as 0 mV (for the ladder
/// that is ENTER, for the battery an empty cell).
pub trait AdcSample {
    fn sample_mv(&mut self) -> Option<u16>;
}

/// `ADC_OVERSAMPLE` samples averaged; any failed sample fails the reading.
pub fn average_mv<A: AdcSample>(adc: &mut A) -> Option<u16> {
    let mut sum: u32 = 0;
    for _ in 0..ADC_OVERSAMPLE {
        sum += adc.sample_mv()? as u32;
    }
    Some((sum / ADC_OVERSAMPLE) as u16)
}

/// A GPIO key pin (pull-up, pressed = low).
pub trait KeyPin {
    fn is_low(&mut self) -> bool;
}

/// Monotonic microsecond clock.
pub trait Clock {
    fn now_us(&mut self) -> u64;
}

// ---- scanner: grace + decode + priority ------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    /// Startup grace running: nothing is sampled, nothing is reported.
    Grace,
    /// Grace over. `latched` is the key that was already down at the first
    /// sample after grace; it is ignored until released (or replaced).
    Armed { latched: Option<Key> },
}

/// Hardware + grace logic behind `InputCore`. Grace applies to every key, like
/// the BSP (board_keys.c:57 drops events from all 7 keys), which is a superset
/// of R13 (front-ladder only). A key already down when grace ends is not
/// reported as a press (the BSP likewise never emits its DOWN).
pub struct KeyScanner<A, P, C> {
    adc: A,
    gpio: [P; 3],
    clock: C,
    start_us: u64,
    phase: Phase,
    now_us: u64,
}

impl<A: AdcSample, P: KeyPin, C: Clock> KeyScanner<A, P, C> {
    fn sample(&mut self) -> Option<Key> {
        let wake = self.gpio[0].is_low();
        let prev = self.gpio[1].is_low();
        let next = self.gpio[2].is_low();
        if let Some(k) = decode_gpio(wake, prev, next) {
            return Some(k);
        }
        average_mv(&mut self.adc).and_then(decode_front_ladder)
    }
}

impl<A: AdcSample, P: KeyPin, C: Clock> RawSource<Key> for KeyScanner<A, P, C> {
    fn read_raw(&mut self) -> Option<Key> {
        let t = self.clock.now_us();
        self.now_us = t;
        match self.phase {
            Phase::Grace => {
                // BSP: `now - start < GRACE` drops the event, so exactly at
                // 2.5 s the grace is over.
                if t.saturating_sub(self.start_us) < STARTUP_GRACE_US {
                    return None;
                }
                let raw = self.sample();
                self.phase = Phase::Armed { latched: raw };
                None
            }
            Phase::Armed { latched } => {
                let raw = self.sample();
                match latched {
                    Some(l) if raw == Some(l) => None,
                    Some(_) => {
                        self.phase = Phase::Armed { latched: None };
                        raw
                    }
                    None => raw,
                }
            }
        }
    }

    fn now_us(&mut self) -> u64 {
        // same instant as the sample (one clock read per poll)
        self.now_us
    }
}

/// The C61 input driver: scanner + shared debounce/long-press/repeat core.
pub struct KeyInput<A, P, C> {
    core: InputCore<Key>,
    scan: KeyScanner<A, P, C>,
}

impl<A: AdcSample, P: KeyPin, C: Clock> KeyInput<A, P, C> {
    /// `gpio` is `[WAKE, PREV, NEXT]`. The grace window starts now (BSP:
    /// `board_keys_init`, board_keys.c:91).
    pub fn new(adc: A, gpio: [P; 3], mut clock: C, timing: InputTiming) -> Self {
        let now = clock.now_us();
        Self {
            core: InputCore::new(timing, now),
            scan: KeyScanner {
                adc,
                gpio,
                clock,
                start_us: now,
                phase: Phase::Grace,
                now_us: now,
            },
        }
    }

    pub fn poll(&mut self) -> Option<Event<Key>> {
        self.core.poll(&mut self.scan)
    }

    /// `poll`, mapped to semantic actions.
    pub fn poll_action(&mut self, swap: bool) -> Option<ActionEvent> {
        self.poll().map(|e| map_event(e, swap))
    }

    pub fn reset_hold_state(&mut self) {
        self.core.reset_hold_state();
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::vec;
    use std::vec::Vec;

    // ---- decoding -----------------------------------------------------------

    #[test]
    fn r12_ladder_bsp_measured_values_decode_to_their_keys() {
        // board_keys.c:32-34: BACK ~2592, LEFT ~1956, RIGHT ~1316, ENTER ~0
        assert_eq!(decode_front_ladder(2592), Some(Key::Back));
        assert_eq!(decode_front_ladder(1956), Some(Key::Left));
        assert_eq!(decode_front_ladder(1316), Some(Key::Right));
        assert_eq!(decode_front_ladder(0), Some(Key::Enter));
    }

    #[test]
    fn r12_ladder_window_edges_are_inclusive_and_one_mv_outside_is_none() {
        for w in FRONT_LADDER {
            assert_eq!(
                decode_front_ladder(w.min_mv),
                Some(w.key),
                "{:?} min",
                w.key
            );
            assert_eq!(
                decode_front_ladder(w.max_mv),
                Some(w.key),
                "{:?} max",
                w.key
            );
            assert_eq!(
                decode_front_ladder(w.min_mv + 1),
                Some(w.key),
                "{:?} min+1",
                w.key
            );
            assert_eq!(
                decode_front_ladder(w.max_mv - 1),
                Some(w.key),
                "{:?} max-1",
                w.key
            );
            if w.min_mv > 0 {
                assert_eq!(decode_front_ladder(w.min_mv - 1), None, "{:?} min-1", w.key);
            }
            assert_eq!(decode_front_ladder(w.max_mv + 1), None, "{:?} max+1", w.key);
        }
    }

    #[test]
    fn r12_ladder_exact_bsp_thresholds() {
        let got: Vec<(Key, u16, u16)> = FRONT_LADDER
            .iter()
            .map(|w| (w.key, w.min_mv, w.max_mv))
            .collect();
        assert_eq!(
            got,
            vec![
                (Key::Back, 2400, 2800),
                (Key::Left, 1780, 2140),
                (Key::Right, 1140, 1500),
                (Key::Enter, 0, 250),
            ]
        );
    }

    #[test]
    fn r12_ladder_rest_and_out_of_range_values_are_no_key() {
        for mv in [2801, 3000, 3100, 3300, 3600, 4095, 4096, 10_000, u16::MAX] {
            assert_eq!(decode_front_ladder(mv), None, "{mv} mV");
        }
    }

    #[test]
    fn r12_ladder_dead_zones_between_keys_are_no_key() {
        // BACK/LEFT, LEFT/RIGHT, RIGHT/ENTER gaps (and rest above BACK)
        for mv in (2141..=2399).chain(1501..=1779).chain(251..=1139) {
            assert_eq!(decode_front_ladder(mv), None, "{mv} mV");
        }
    }

    #[test]
    fn r12_ladder_windows_are_ordered_and_disjoint_with_gaps() {
        for pair in FRONT_LADDER.windows(2) {
            assert!(
                pair[0].min_mv > pair[1].max_mv,
                "{:?} vs {:?}",
                pair[0],
                pair[1]
            );
        }
        for w in FRONT_LADDER {
            assert!(w.min_mv <= w.max_mv);
        }
    }

    #[test]
    fn r12_ladder_every_mv_maps_to_the_one_window_that_contains_it() {
        for mv in 0..=u16::MAX {
            let owners: Vec<Key> = FRONT_LADDER
                .iter()
                .filter(|w| w.min_mv <= mv && mv <= w.max_mv)
                .map(|w| w.key)
                .collect();
            assert!(owners.len() <= 1);
            assert_eq!(decode_front_ladder(mv), owners.first().copied(), "{mv}");
        }
    }

    #[test]
    fn r12_gpio_decode_priority_wake_prev_next_over_all_combinations() {
        for bits in 0..8u8 {
            let (w, p, n) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
            let want = if w {
                Some(Key::Wake)
            } else if p {
                Some(Key::Prev)
            } else if n {
                Some(Key::Next)
            } else {
                None
            };
            assert_eq!(decode_gpio(w, p, n), want, "bits {bits:03b}");
        }
    }

    // ---- key -> action table -----------------------------------------------

    #[test]
    fn r12_key_to_action_table_default_layout() {
        use Action::*;
        let got: Vec<(Key, Action)> = Key::ALL.iter().map(|k| (*k, k.action(false))).collect();
        assert_eq!(
            got,
            vec![
                (Key::Wake, Back),
                (Key::Prev, Prev),
                (Key::Next, Next),
                (Key::Back, Back),
                (Key::Left, PrevJump),
                (Key::Right, NextJump),
                (Key::Enter, Select),
            ]
        );
    }

    #[test]
    fn r12_key_to_action_table_swapped_layout() {
        use Action::*;
        let got: Vec<(Key, Action)> = Key::ALL.iter().map(|k| (*k, k.action(true))).collect();
        // X4 ButtonMapper swap: Back<->Left, Confirm<->Right; side keys fixed
        assert_eq!(
            got,
            vec![
                (Key::Wake, Back),
                (Key::Prev, Prev),
                (Key::Next, Next),
                (Key::Back, PrevJump),
                (Key::Left, Back),
                (Key::Right, Select),
                (Key::Enter, NextJump),
            ]
        );
    }

    #[test]
    fn r12_every_existing_action_but_menu_is_reachable_and_none_is_new() {
        for swap in [false, true] {
            let reach: Vec<Action> = Key::ALL.iter().map(|k| k.action(swap)).collect();
            for a in [
                Action::Next,
                Action::Prev,
                Action::NextJump,
                Action::PrevJump,
                Action::Select,
                Action::Back,
            ] {
                assert!(reach.contains(&a), "swap={swap}: {a:?} unreachable");
            }
            // documented trade-off: no physical Menu key on the C61
            assert!(!reach.contains(&Action::Menu));
        }
    }

    #[test]
    fn r12_map_event_keeps_the_event_kind() {
        use crate::input::Event as E;
        assert_eq!(
            map_event(E::Press(Key::Left), false),
            ActionEvent::Press(Action::PrevJump)
        );
        assert_eq!(
            map_event(E::Release(Key::Enter), false),
            ActionEvent::Release(Action::Select)
        );
        assert_eq!(
            map_event(E::LongPress(Key::Back), false),
            ActionEvent::LongPress(Action::Back)
        );
        assert_eq!(
            map_event(E::Repeat(Key::Next), true),
            ActionEvent::Repeat(Action::Next)
        );
        assert_eq!(
            map_event(E::Press(Key::Left), true),
            ActionEvent::Press(Action::Back)
        );
    }

    #[test]
    fn key_names_follow_the_bsp() {
        let names: Vec<&str> = Key::ALL.iter().map(|k| k.name()).collect();
        assert_eq!(
            names,
            vec!["WAKE", "PREV", "NEXT", "BACK", "LEFT", "RIGHT", "ENTER"]
        );
    }

    #[test]
    fn r12_gpio_keys_are_active_low() {
        assert!(GPIO_KEYS_ACTIVE_LOW);
    }

    // ---- fake hardware ------------------------------------------------------

    struct Hw {
        mv: Cell<Option<u16>>,
        low: [Cell<bool>; 3], // [WAKE, PREV, NEXT]
        t_us: Cell<u64>,
        adc_samples: Cell<u32>,
        pin_reads: Cell<u32>,
    }
    struct FAdc(Rc<Hw>);
    struct FPin(Rc<Hw>, usize);
    struct FClock(Rc<Hw>);
    impl AdcSample for FAdc {
        fn sample_mv(&mut self) -> Option<u16> {
            self.0.adc_samples.set(self.0.adc_samples.get() + 1);
            self.0.mv.get()
        }
    }
    impl KeyPin for FPin {
        fn is_low(&mut self) -> bool {
            self.0.pin_reads.set(self.0.pin_reads.get() + 1);
            self.0.low[self.1].get()
        }
    }
    impl Clock for FClock {
        fn now_us(&mut self) -> u64 {
            self.0.t_us.get()
        }
    }

    const REST: Option<u16> = Some(3100);
    /// The board's clock is already running when the keys are created.
    const BOOT_US: u64 = 700_000;
    const GRACE_MS: u64 = 2500;

    struct Rig {
        hw: Rc<Hw>,
        input: KeyInput<FAdc, FPin, FClock>,
    }

    impl Rig {
        fn new() -> Self {
            Self::starting_at(BOOT_US)
        }
        fn starting_at(t_us: u64) -> Self {
            let hw = Rc::new(Hw {
                mv: Cell::new(REST),
                low: [Cell::new(false), Cell::new(false), Cell::new(false)],
                t_us: Cell::new(t_us),
                adc_samples: Cell::new(0),
                pin_reads: Cell::new(0),
            });
            let input = KeyInput::new(
                FAdc(hw.clone()),
                [
                    FPin(hw.clone(), 0),
                    FPin(hw.clone(), 1),
                    FPin(hw.clone(), 2),
                ],
                FClock(hw.clone()),
                InputTiming::PULP,
            );
            Self { hw, input }
        }
        fn start_us(&self) -> u64 {
            BOOT_US
        }
        /// Poll once at `ms` milliseconds after the keys were created.
        fn poll_at_us(&mut self, us_after_start: u64) -> Option<Event<Key>> {
            self.hw.t_us.set(self.start_us() + us_after_start);
            self.input.poll()
        }
        fn poll_at(&mut self, ms: u64) -> Option<Event<Key>> {
            self.poll_at_us(ms * 1000)
        }
        /// Poll every `step` ms over `[from, to]`, collecting (ms, event).
        fn run(&mut self, from: u64, to: u64, step: u64) -> Vec<(u64, Event<Key>)> {
            let mut out = Vec::new();
            let mut t = from;
            while t <= to {
                if let Some(e) = self.poll_at(t) {
                    out.push((t, e));
                }
                t += step;
            }
            out
        }
        fn set_ladder(&self, mv: Option<u16>) {
            self.hw.mv.set(mv);
        }
        fn set_gpio(&self, wake: bool, prev: bool, next: bool) {
            self.hw.low[0].set(wake);
            self.hw.low[1].set(prev);
            self.hw.low[2].set(next);
        }
        /// Run through the grace period with no key down.
        fn past_grace(&mut self) {
            assert_eq!(self.run(0, GRACE_MS + 20, 10), vec![]);
        }
    }

    // ---- ADC averaging / failure -------------------------------------------

    struct Seq(Vec<Option<u16>>, usize);
    impl AdcSample for Seq {
        fn sample_mv(&mut self) -> Option<u16> {
            let v = self.0[self.1 % self.0.len()];
            self.1 += 1;
            v
        }
    }

    #[test]
    fn r12_average_is_the_truncated_mean_of_oversample_reads() {
        let mut a = Seq(vec![Some(1000), Some(1001), Some(1001), Some(1001)], 0);
        assert_eq!(average_mv(&mut a), Some(1000)); // 4003 / 4 = 1000
        assert_eq!(a.1, ADC_OVERSAMPLE as usize);
        let mut b = Seq(vec![Some(65535); 4], 0);
        assert_eq!(average_mv(&mut b), Some(65535), "u32 sum, no overflow");
    }

    #[test]
    fn r12_adc_failure_is_not_zero_mv_and_not_enter() {
        let mut a = Seq(vec![Some(0), Some(0), None, Some(0)], 0);
        assert_eq!(average_mv(&mut a), None);

        // through the whole path: a failing ADC after grace never yields ENTER
        let mut r = Rig::new();
        r.past_grace();
        r.set_ladder(None);
        assert_eq!(r.run(2600, 3200, 5), vec![]);
        // while a real 0 mV reading is ENTER
        r.set_ladder(Some(0));
        let ev = r.run(3205, 3300, 5);
        assert_eq!(ev, vec![(3220, Event::Press(Key::Enter))]);
    }

    // ---- ladder through the driver (R12) -----------------------------------

    #[test]
    fn r12_each_ladder_key_press_release_after_grace() {
        for (mv, key) in [
            (2592u16, Key::Back),
            (1956, Key::Left),
            (1316, Key::Right),
            (0, Key::Enter),
        ] {
            let mut r = Rig::new();
            r.past_grace();
            r.set_ladder(Some(mv));
            let down = r.run(2600, 2700, 5);
            assert_eq!(down, vec![(2615, Event::Press(key))], "{key:?}");
            r.set_ladder(REST);
            let up = r.run(2705, 2800, 5);
            assert_eq!(up, vec![(2720, Event::Release(key))], "{key:?}");
        }
    }

    #[test]
    fn r12_ladder_transient_across_bands_does_not_fire_a_neighbour() {
        // BSP comment: a press/release sweeping across levels must land in a
        // dead zone, not in another key's band. 6 ms in each band < debounce.
        let mut r = Rig::new();
        r.past_grace();
        let mut t = 2600;
        for mv in [
            2592u16, 2300, 1956, 1600, 1316, 700, 0, 700, 1316, 1600, 1956, 2300, 2592, 3100,
        ] {
            r.set_ladder(Some(mv));
            assert_eq!(r.run(t, t + 5, 1), vec![], "mv {mv}");
            t += 6;
        }
        assert_eq!(r.run(t, t + 100, 5), vec![]);
    }

    #[test]
    fn r12_ladder_long_press_and_repeat_via_the_shared_core() {
        let mut r = Rig::new();
        r.past_grace();
        r.set_ladder(Some(2592)); // BACK
        let ev = r.run(2600, 2600 + 1015 + 150, 5);
        assert_eq!(
            ev,
            vec![
                (2615, Event::Press(Key::Back)),
                (3615, Event::LongPress(Key::Back)),
                (3765, Event::Repeat(Key::Back)),
            ]
        );
    }

    #[test]
    fn r12_poll_action_maps_through_the_table() {
        let mut r = Rig::new();
        r.past_grace();
        r.set_ladder(Some(1956)); // LEFT
        let mut got = None;
        for t in (2600..=2700).step_by(5) {
            r.hw.t_us.set(r.start_us() + t * 1000);
            if let Some(e) = r.input.poll_action(false) {
                got = Some(e);
            }
        }
        assert_eq!(got, Some(ActionEvent::Press(Action::PrevJump)));
    }

    // ---- GPIO keys ------------------------------------------------------------

    #[test]
    fn r12_each_gpio_key_is_pressed_when_its_pin_is_low() {
        for (i, key) in [Key::Wake, Key::Prev, Key::Next].into_iter().enumerate() {
            let mut r = Rig::new();
            r.past_grace();
            r.set_gpio(i == 0, i == 1, i == 2);
            assert_eq!(
                r.run(2600, 2700, 5),
                vec![(2615, Event::Press(key))],
                "{key:?}"
            );
            r.set_gpio(false, false, false);
            assert_eq!(
                r.run(2705, 2800, 5),
                vec![(2720, Event::Release(key))],
                "{key:?}"
            );
        }
    }

    #[test]
    fn r12_gpio_high_pin_is_released_not_pressed() {
        let mut r = Rig::new();
        r.past_grace();
        // pins read "not low" (pull-up) and the ladder at rest: nothing
        assert_eq!(r.run(2600, 4000, 5), vec![]);
    }

    #[test]
    fn r12_two_gpio_keys_report_the_higher_priority_only() {
        let mut r = Rig::new();
        r.past_grace();
        r.set_gpio(false, true, true); // PREV + NEXT
        assert_eq!(r.run(2600, 2700, 5), vec![(2615, Event::Press(Key::Prev))]);
        // adding WAKE takes over (priority), then releasing it hands back
        r.set_gpio(true, true, true);
        let ev = r.run(2705, 2800, 5);
        assert_eq!(
            ev,
            vec![
                (2720, Event::Release(Key::Prev)),
                (2725, Event::Press(Key::Wake))
            ]
        );
        r.set_gpio(false, true, true);
        let ev = r.run(2805, 2900, 5);
        assert_eq!(
            ev,
            vec![
                (2820, Event::Release(Key::Wake)),
                (2825, Event::Press(Key::Prev))
            ]
        );
    }

    #[test]
    fn r12_ladder_plus_gpio_the_gpio_key_wins_and_skips_the_adc() {
        let mut r = Rig::new();
        r.past_grace();
        r.set_ladder(Some(2592)); // BACK held
        assert_eq!(r.run(2600, 2650, 5), vec![(2615, Event::Press(Key::Back))]);
        let samples = r.hw.adc_samples.get();
        r.set_gpio(false, false, true); // NEXT joins
        let ev = r.run(2655, 2750, 5);
        assert_eq!(
            ev,
            vec![
                (2670, Event::Release(Key::Back)),
                (2675, Event::Press(Key::Next))
            ]
        );
        // once NEXT is the only candidate the ladder is not sampled any more
        let after_switch = r.hw.adc_samples.get();
        r.run(2755, 2800, 5);
        assert_eq!(r.hw.adc_samples.get(), after_switch);
        assert!(after_switch >= samples);
        // NEXT released while BACK is still down: BACK is pressed again
        r.set_gpio(false, false, false);
        let ev = r.run(2805, 2900, 5);
        assert_eq!(
            ev,
            vec![
                (2820, Event::Release(Key::Next)),
                (2825, Event::Press(Key::Back))
            ]
        );
    }

    // ---- startup grace (R13) ----------------------------------------------------

    #[test]
    fn r13_grace_is_two_and_a_half_seconds() {
        assert_eq!(STARTUP_GRACE_US, 2_500_000); // board_keys.c:22
    }

    #[test]
    fn r13_enter_band_reading_during_grace_emits_nothing() {
        // BSP: the node reads ~0 mV (= ENTER) while it settles after boot
        let mut r = Rig::new();
        r.set_ladder(Some(0));
        assert_eq!(r.run(0, 2499, 1), vec![]);
        assert_eq!(r.hw.adc_samples.get(), 0, "ladder is not even sampled");
    }

    #[test]
    fn r13_every_front_key_value_during_grace_emits_nothing() {
        for mv in [0u16, 100, 1316, 1956, 2592, 3100] {
            let mut r = Rig::new();
            r.set_ladder(Some(mv));
            assert_eq!(r.run(0, 2499, 1), vec![], "{mv} mV");
        }
    }

    #[test]
    fn r13_long_hold_during_grace_never_long_presses() {
        let mut r = Rig::new();
        r.set_ladder(Some(2592));
        // 2.5 s hold >> debounce (15 ms) and long-press (1 s): still silent
        assert_eq!(r.run(0, 2499, 5), vec![]);
    }

    #[test]
    fn r13_grace_boundary_one_microsecond_before_and_exactly_at_2500ms() {
        let mut r = Rig::new();
        r.set_ladder(Some(2592));
        assert_eq!(r.poll_at_us(2_499_999), None);
        assert_eq!(r.hw.adc_samples.get(), 0, "still inside grace: not sampled");
        assert_eq!(r.poll_at_us(2_500_000), None);
        assert!(
            r.hw.adc_samples.get() > 0,
            "exactly 2.5 s is outside grace: the node is sampled"
        );
    }

    #[test]
    fn r13_grace_runs_from_creation_not_from_clock_zero() {
        // keys created at an arbitrary late clock value: grace still 2.5 s
        let mut r = Rig::starting_at(90_000_000);
        r.hw.t_us.set(90_000_000);
        r.set_ladder(Some(2592));
        r.hw.t_us.set(90_000_000 + 2_499_999);
        assert_eq!(r.input.poll(), None);
        assert_eq!(r.hw.adc_samples.get(), 0);
        r.hw.t_us.set(90_000_000 + 2_500_000);
        let _ = r.input.poll();
        assert!(r.hw.adc_samples.get() > 0);
    }

    #[test]
    fn r13_key_held_through_grace_end_is_ignored_until_released() {
        // ladder key down from boot (also: wake key still held after wake-up)
        let mut r = Rig::new();
        r.set_ladder(Some(1956));
        assert_eq!(
            r.run(0, 4000, 5),
            vec![],
            "held past grace: no Press, no LongPress"
        );
        r.set_ladder(REST);
        assert_eq!(
            r.run(4005, 4100, 5),
            vec![],
            "its release is not reported either"
        );
        r.set_ladder(Some(1956));
        assert_eq!(
            r.run(4105, 4200, 5),
            vec![(4120, Event::Press(Key::Left))],
            "a fresh press afterwards works"
        );
    }

    #[test]
    fn r13_latched_key_is_replaced_by_a_different_key() {
        let mut r = Rig::new();
        r.set_ladder(Some(1956)); // LEFT held across the end of grace
        assert_eq!(r.run(0, 3000, 5), vec![]);
        r.set_ladder(Some(2592)); // finger slides to BACK
        let ev = r.run(3005, 3100, 5);
        assert_eq!(ev, vec![(3020, Event::Press(Key::Back))]);
    }

    #[test]
    fn r13_key_pressed_just_after_grace_is_reported() {
        let mut r = Rig::new();
        assert_eq!(r.run(0, 2500, 10), vec![]); // first post-grace sample: rest
        r.set_ladder(Some(2592));
        let ev = r.run(2510, 2600, 5);
        assert_eq!(ev, vec![(2525, Event::Press(Key::Back))]);
    }

    #[test]
    fn r13_gpio_keys_are_suppressed_during_grace_like_the_bsp() {
        // BSP btn_trampoline drops events from ALL keys during the window
        // (board_keys.c:57); R13 only requires the front ladder, this is the
        // stricter, BSP-faithful superset.
        for pin in 0..3 {
            let mut r = Rig::new();
            r.set_gpio(pin == 0, pin == 1, pin == 2);
            assert_eq!(r.run(0, 2499, 5), vec![], "pin {pin}");
            assert_eq!(r.hw.pin_reads.get(), 0, "pins are not sampled in grace");
        }
    }

    #[test]
    fn r13_gpio_key_held_from_wake_up_through_grace_is_ignored_until_released() {
        let mut r = Rig::new();
        r.set_gpio(true, false, false); // WAKE still down after waking
        assert_eq!(r.run(0, 3500, 5), vec![]);
        r.set_gpio(false, false, false);
        assert_eq!(r.run(3505, 3600, 5), vec![]);
        r.set_gpio(true, false, false);
        assert_eq!(r.run(3605, 3700, 5), vec![(3620, Event::Press(Key::Wake))]);
    }

    #[test]
    fn r13_gpio_key_pressed_after_grace_is_reported() {
        let mut r = Rig::new();
        r.past_grace();
        r.set_gpio(false, false, true);
        assert_eq!(r.run(2600, 2700, 5), vec![(2615, Event::Press(Key::Next))]);
    }
}
