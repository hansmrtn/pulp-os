// USB insertion detect (R17), HAL-free. GPIO11, input with internal pull-up.
//
// POLARITY IS A CONFIGURATION VALUE (`USB_POLARITY`), never hard-coded in the
// decode path. Sources disagree, so the choice and its evidence are recorded
// here (and in specs/changes/onepage-c61-port/baseline.md, T9):
//
//   source                                              says
//   --------------------------------------------------- ----------------------
//   BSP implementation board_c61.c:306                  plugged = level LOW
//     `gpio_get_level(PIN_USB_DET) == 0`
//   BSP comments board_c61.c:61, :272-273               LM66200 ST open-drain,
//                                                       ext pull-up: low = USB
//   BSP README.md:52 "USB insertion detection           plugged = level HIGH
//     (high = plugged)"                                 (CONTRADICTS the code)
//   schematic SCH_Sch_OnePage_V1_2026-08-19.pdf         U12 LM66200DRLR pin 8
//     (../onepage-reader/electronics/c61), PSW block    ST -> USB_DET, R29 10k
//                                                       pull-up to +3V3, note
//                                                       "USB=0" next to U12
//   crosspoint-onepage lib/hal/HalGPIO.cpp:245-249,     LOW = USB present; both
//     :368-370                                          comments claim "Verified
//                                                       on hardware" (BATDIAG
//                                                       read LOW with USB in)
//   LM66200 datasheet behaviour (not in this tree):     ST is open-drain and
//                                                       pulled low when active
//                                                       (not read here)
//
// Default = ActiveLow: the BSP code, the BSP's own comments, the schematic and
// a second firmware's hardware notes agree; only the README sentence differs
// (a documentation slip is the likelier explanation than a code bug, since the
// README sentence is the only one that does not match the circuit's pull-up).
// STILL TO CONFIRM ON HARDWARE: plug and unplug USB with the C61 build and
// read the logged level. If it is wrong, change `USB_POLARITY` only; the tests
// run both polarities.
//
// Debounce: the BSP has none (it reads the level on demand). USB_DEBOUNCE_SAMPLES
// is a deliberate addition, so a cable that chatters while seating does not
// emit plug/unplug bursts. Set it to 1 for BSP-exact immediate behaviour.

/// How GPIO11's level maps to "USB plugged".
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UsbPolarity {
    /// High level = USB present.
    ActiveHigh,
    /// Low level = USB present (LM66200 ST pulls low when USB power is valid).
    ActiveLow,
}

impl UsbPolarity {
    pub const fn plugged(self, level_high: bool) -> bool {
        match self {
            UsbPolarity::ActiveHigh => level_high,
            UsbPolarity::ActiveLow => !level_high,
        }
    }
}

/// The configured polarity of this board. See the table above; unconfirmed on
/// hardware.
pub const USB_POLARITY: UsbPolarity = UsbPolarity::ActiveLow;

/// Consecutive samples that must disagree with the current state before a
/// change is accepted. Not in the BSP (see above).
pub const USB_DEBOUNCE_SAMPLES: u8 = 3;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UsbEvent {
    Plugged,
    Unplugged,
}

/// Debounced USB state with change events. There is no "unknown" state: the
/// first raw sample defines the initial state (no event for it), so nothing
/// is assumed before the pin has been read.
#[derive(Copy, Clone, Debug)]
pub struct UsbDetect {
    polarity: UsbPolarity,
    debounce: u8,
    plugged: bool,
    pending: u8,
}

impl UsbDetect {
    /// `first_level_high` is the raw GPIO11 level read right now. A
    /// `debounce_samples` of 0 behaves like 1 (no debounce).
    pub const fn new(polarity: UsbPolarity, debounce_samples: u8, first_level_high: bool) -> Self {
        Self {
            polarity,
            debounce: if debounce_samples == 0 {
                1
            } else {
                debounce_samples
            },
            plugged: polarity.plugged(first_level_high),
            pending: 0,
        }
    }

    /// Detector with the board configuration (`USB_POLARITY`,
    /// `USB_DEBOUNCE_SAMPLES`).
    pub const fn for_board(first_level_high: bool) -> Self {
        Self::new(USB_POLARITY, USB_DEBOUNCE_SAMPLES, first_level_high)
    }

    pub const fn polarity(&self) -> UsbPolarity {
        self.polarity
    }

    pub const fn plugged(&self) -> bool {
        self.plugged
    }

    /// Feed one raw GPIO11 sample. Returns an event only when the debounced
    /// state changes.
    pub fn sample(&mut self, level_high: bool) -> Option<UsbEvent> {
        let observed = self.polarity.plugged(level_high);
        if observed == self.plugged {
            self.pending = 0;
            return None;
        }
        self.pending += 1;
        if self.pending < self.debounce {
            return None;
        }
        self.pending = 0;
        self.plugged = observed;
        Some(if observed {
            UsbEvent::Plugged
        } else {
            UsbEvent::Unplugged
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    const BOTH: [UsbPolarity; 2] = [UsbPolarity::ActiveHigh, UsbPolarity::ActiveLow];

    #[test]
    fn r17_mapping_table_for_both_polarities() {
        //            level  ActiveHigh ActiveLow
        let table = [(true, true, false), (false, false, true)];
        for (level, high, low) in table {
            assert_eq!(
                UsbPolarity::ActiveHigh.plugged(level),
                high,
                "high {}",
                level
            );
            assert_eq!(UsbPolarity::ActiveLow.plugged(level), low, "low {}", level);
        }
    }

    #[test]
    fn r17_board_polarity_is_active_low_per_bsp_code_and_schematic() {
        // pinned so that a change of the configuration is a visible edit
        assert_eq!(USB_POLARITY, UsbPolarity::ActiveLow);
        assert!(
            !UsbDetect::for_board(true).plugged(),
            "pull-up high = unplugged"
        );
        assert!(
            UsbDetect::for_board(false).plugged(),
            "ST pulled low = plugged"
        );
    }

    #[test]
    fn r17_flipping_the_polarity_flips_every_result() {
        // proves the setting is honoured by the decode path, not baked in
        for level in [false, true] {
            let a = UsbDetect::new(UsbPolarity::ActiveHigh, 1, level);
            let b = UsbDetect::new(UsbPolarity::ActiveLow, 1, level);
            assert_ne!(a.plugged(), b.plugged(), "level {}", level);
        }
        // same raw trace, opposite events
        let trace = [true, true, false, false, true];
        let run = |p| {
            let mut d = UsbDetect::new(p, 1, true);
            trace
                .iter()
                .filter_map(|l| d.sample(*l))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            run(UsbPolarity::ActiveHigh),
            [UsbEvent::Unplugged, UsbEvent::Plugged]
        );
        assert_eq!(
            run(UsbPolarity::ActiveLow),
            [UsbEvent::Plugged, UsbEvent::Unplugged]
        );
    }

    #[test]
    fn r17_initial_state_comes_from_the_first_sample_without_an_event() {
        for p in BOTH {
            for level in [false, true] {
                let d = UsbDetect::new(p, 3, level);
                assert_eq!(d.plugged(), p.plugged(level));
                assert_eq!(d.polarity(), p);
            }
        }
    }

    #[test]
    fn r17_events_only_when_the_state_changes() {
        for p in BOTH {
            for first in [false, true] {
                let mut d = UsbDetect::new(p, 3, first);
                for _ in 0..100 {
                    assert_eq!(d.sample(first), None, "steady {:?} {}", p, first);
                }
            }
        }
    }

    #[test]
    fn r17_change_is_reported_once_with_the_right_direction() {
        let mut d = UsbDetect::new(UsbPolarity::ActiveLow, 1, true); // unplugged
        assert_eq!(d.sample(false), Some(UsbEvent::Plugged));
        assert!(d.plugged());
        assert_eq!(d.sample(false), None);
        assert_eq!(d.sample(true), Some(UsbEvent::Unplugged));
        assert!(!d.plugged());
        assert_eq!(d.sample(true), None);
    }

    #[test]
    fn r17_debounce_needs_n_consecutive_disagreeing_samples() {
        let mut d = UsbDetect::new(UsbPolarity::ActiveLow, 3, true);
        assert_eq!(d.sample(false), None);
        assert_eq!(d.sample(false), None);
        assert!(!d.plugged(), "still unplugged after 2 of 3");
        assert_eq!(d.sample(false), Some(UsbEvent::Plugged));
        assert!(d.plugged());
    }

    #[test]
    fn r17_debounce_glitch_resets_the_count() {
        let mut d = UsbDetect::new(UsbPolarity::ActiveLow, 3, true);
        assert_eq!(d.sample(false), None);
        assert_eq!(d.sample(false), None);
        assert_eq!(d.sample(true), None); // glitch back: restart
        assert_eq!(d.sample(false), None);
        assert_eq!(d.sample(false), None);
        assert_eq!(d.sample(false), Some(UsbEvent::Plugged));
    }

    #[test]
    fn r17_debounce_applies_to_unplug_too() {
        let mut d = UsbDetect::new(UsbPolarity::ActiveLow, 3, false); // plugged
        assert_eq!(d.sample(true), None);
        assert_eq!(d.sample(true), None);
        assert_eq!(d.sample(true), Some(UsbEvent::Unplugged));
    }

    #[test]
    fn r17_zero_and_one_debounce_are_immediate() {
        for n in [0u8, 1] {
            let mut d = UsbDetect::new(UsbPolarity::ActiveHigh, n, false);
            assert_eq!(d.sample(true), Some(UsbEvent::Plugged));
            assert_eq!(d.sample(false), Some(UsbEvent::Unplugged));
        }
    }

    #[test]
    fn r17_board_debounce_constant_is_used_by_for_board() {
        let mut d = UsbDetect::for_board(true);
        for _ in 0..USB_DEBOUNCE_SAMPLES - 1 {
            assert_eq!(d.sample(false), None);
        }
        assert_eq!(d.sample(false), Some(UsbEvent::Plugged));
    }

    #[test]
    fn r17_pin_is_gpio11_in_the_shared_pin_map() {
        assert_eq!(crate::pins::USB_DETECT, 11);
    }
}
