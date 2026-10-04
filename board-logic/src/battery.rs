// Battery sampling and charge-pause contract (R16), HAL-free.
//
// Hardware (OnePage C61):
//   * battery sense = GPIO5 = ADC1_CH3 behind a 1:1 divider: 5.1 MOhm (R7,
//     VBAT -> BAT_ADC) + 5.1 MOhm (R12, BAT_ADC -> GND) + 100 nF (C22) in the
//     schematic (../onepage-reader/electronics/c61/SCH_Sch_OnePage_V1_2026-08-19.pdf,
//     CHARGE block), matching the BSP's x2 (board_c61.c:303, README.md:158);
//   * charge control = GPIO10 -> charger CE pin (R19 10 kOhm pull-up to 3V3, so
//     the charger is enabled unless GPIO10 pulls it low). High = charging
//     allowed, low = paused (board_c61.c:60, :270, :291, :299; crosspoint
//     HalPowerManager.cpp:153-158).
//
// Contract copied from the BSP's `board_battery_mv()` (board_c61.c:289-304):
//   1. pause charging (GPIO10 low) so the charger's terminal voltage does not
//      inflate the reading ("pull low to read true battery V", :60);
//   2. wait CHARGE_SETTLE_MS (30 ms, :292);
//   3. take SAMPLE_COUNT (16, :295) ADC reads and average them;
//   4. resume charging (GPIO10 high, :299);
//   5. convert with the x2 divider (:303).
//
// Deliberate differences from the BSP (not hardware facts, documented in
// baseline.md T9):
//   * a failed ADC read is an ERROR, not "skip and still divide by 16" (the BSP
//     counts a failed read as 0 and so under-reports) and never "0 V";
//   * charging is resumed by a drop guard, so it is restored on every exit
//     path, including a panic in a sampler;
//   * the average is over calibrated mV (esp-hal `read_oneshot` returns mV with
//     curve-fitting calibration) instead of raw counts converted afterwards.
//
// The charge pin is owned by `BatteryMonitor` and only lent, exclusively, to
// the `ChargePause` guard for the duration of one measurement, so no other code
// path can write GPIO10 between pause and resume.

use crate::keys::AdcSample;
use crate::power::DelayMs;

/// ADC1 channel of GPIO5. BSP board_c61.c:63; esp-hal's own table agrees
/// (esp-metadata-generated 0.5.3 `_generated_esp32c61.rs`, GPIO5 = ADC1_CH3).
pub const BATTERY_ADC_CHANNEL: u8 = 3;
/// Wait between pausing the charger and the first sample, ms (board_c61.c:292).
pub const CHARGE_SETTLE_MS: u32 = 30;
/// ADC reads averaged per measurement (board_c61.c:295).
pub const SAMPLE_COUNT: u32 = 16;
/// Voltage-divider multiplier, 1:1 divider (board_c61.c:303; schematic R7/R12).
pub const DIVIDER_MULT: u32 = 2;

/// Piecewise-linear Li-ion discharge curve, descending by mV. This is the X4
/// table verbatim (kernel/src/board/battery.rs before T9). The BSP has no
/// percentage curve and crosspoint's `BatteryMonitor` source is not in the
/// tree, so the C61 reuses this one: UNVERIFIED for the C61's cell.
pub const LIPO_DISCHARGE_CURVE: &[(u32, u8)] = &[
    (4200, 100),
    (4060, 90),
    (3980, 80),
    (3920, 70),
    (3870, 60),
    (3830, 50),
    (3790, 40),
    (3750, 30),
    (3700, 20),
    (3600, 10),
    (3400, 5),
    (3000, 0),
];

/// Voltage -> percent on a descending piecewise-linear curve. Moved verbatim
/// out of the X4 `drivers::battery::battery_percentage` (clamped at both ends,
/// integer floor in between); an empty curve reads 0.
pub fn percentage_from_curve(curve: &[(u32, u8)], mv: u32) -> u8 {
    if curve.is_empty() {
        return 0;
    }
    if mv >= curve[0].0 {
        return curve[0].1;
    }

    let last = curve.len() - 1;
    if mv <= curve[last].0 {
        return curve[last].1;
    }

    let mut i = 0;
    while i + 1 < curve.len() {
        let (mv_hi, pct_hi) = curve[i];
        let (mv_lo, pct_lo) = curve[i + 1];
        if mv >= mv_lo {
            let span_mv = mv_hi - mv_lo;
            if span_mv == 0 {
                return pct_hi;
            }
            let span_pct = (pct_hi - pct_lo) as u32;
            let frac = mv - mv_lo;
            return (pct_lo as u32 + frac * span_pct / span_mv) as u8;
        }
        i += 1;
    }

    0
}

/// Percent for the C61 cell (shared curve).
pub fn percentage(cell_mv: u16) -> u8 {
    percentage_from_curve(LIPO_DISCHARGE_CURVE, cell_mv as u32)
}

/// ADC pin mV -> cell mV with the 1:1 divider. Saturates instead of wrapping.
pub const fn cell_mv_from_adc_mv(adc_mv: u16) -> u16 {
    let v = adc_mv as u32 * DIVIDER_MULT;
    if v > u16::MAX as u32 {
        u16::MAX
    } else {
        v as u16
    }
}

/// Charge-enable output (GPIO10). `true` = charging allowed.
pub trait ChargePin {
    fn set_charging(&mut self, enabled: bool);
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BatteryError {
    /// ADC read number `sample` (0-based) failed. Charging was resumed.
    AdcFailed { sample: u8 },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BatteryReading {
    /// Truncated mean of the calibrated ADC reads, mV at the ADC pin.
    pub adc_mv: u16,
    /// Cell voltage, mV (x2 divider).
    pub cell_mv: u16,
}

impl BatteryReading {
    pub fn percent(&self) -> u8 {
        percentage(self.cell_mv)
    }
}

/// Charging is paused for exactly the lifetime of this guard. The guard holds
/// the only `&mut` to the pin, and `Drop` resumes charging, so every exit path
/// restores it.
struct ChargePause<'a, C: ChargePin> {
    pin: &'a mut C,
}

impl<'a, C: ChargePin> ChargePause<'a, C> {
    fn new(pin: &'a mut C) -> Self {
        pin.set_charging(false);
        Self { pin }
    }
}

impl<C: ChargePin> Drop for ChargePause<'_, C> {
    fn drop(&mut self) {
        self.pin.set_charging(true);
    }
}

fn sample_sum<A: AdcSample>(adc: &mut A) -> Result<u32, BatteryError> {
    let mut sum: u32 = 0;
    for i in 0..SAMPLE_COUNT {
        match adc.sample_mv() {
            Some(mv) => sum += mv as u32,
            None => return Err(BatteryError::AdcFailed { sample: i as u8 }),
        }
    }
    Ok(sum)
}

/// Battery measurement owner. `measure` takes `&mut self`, so two overlapping
/// measurements (and any foreign write to GPIO10 during one) are impossible.
pub struct BatteryMonitor<C: ChargePin, A: AdcSample, D: DelayMs> {
    charge: C,
    adc: A,
    delay: D,
}

impl<C: ChargePin, A: AdcSample, D: DelayMs> BatteryMonitor<C, A, D> {
    /// Takes the pins and drives charging enabled (BSP default,
    /// board_c61.c:270 "allow charging by default").
    pub fn new(mut charge: C, adc: A, delay: D) -> Self {
        charge.set_charging(true);
        Self { charge, adc, delay }
    }

    /// Drive charging enabled (idempotent). `measure` already resumes charging
    /// on every exit path, so this is a belt-and-braces step of the deep-sleep
    /// entry (sleep_enter does not touch GPIO10 itself in the BSP): the last
    /// thing done to the pin before sleep is "charging allowed", never "paused".
    pub fn ensure_charging(&mut self) {
        self.charge.set_charging(true);
    }

    /// pause -> settle -> sample -> resume -> convert. Charging is resumed
    /// before the result is computed and also when a read fails.
    pub fn measure(&mut self) -> Result<BatteryReading, BatteryError> {
        let sum = {
            let _pause = ChargePause::new(&mut self.charge);
            self.delay.delay_ms(CHARGE_SETTLE_MS);
            sample_sum(&mut self.adc)
        }?;
        let adc_mv = (sum / SAMPLE_COUNT) as u16;
        Ok(BatteryReading {
            adc_mv,
            cell_mv: cell_mv_from_adc_mv(adc_mv),
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::vec;
    use std::vec::Vec;

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    enum Ev {
        Charge(bool),
        Delay(u32),
        Sample(u8),
        SampleFail(u8),
    }

    type Log = Rc<RefCell<Vec<Ev>>>;

    struct FakeCharge(Log);
    impl ChargePin for FakeCharge {
        fn set_charging(&mut self, enabled: bool) {
            self.0.borrow_mut().push(Ev::Charge(enabled));
        }
    }

    struct FakeDelay(Log);
    impl DelayMs for FakeDelay {
        fn delay_ms(&mut self, ms: u32) {
            self.0.borrow_mut().push(Ev::Delay(ms));
        }
    }

    /// Scripted ADC: `values[i]` for read i; `None` = failed read; reads past
    /// the script repeat the last entry. `panic_at` makes read i panic.
    struct FakeAdc {
        log: Log,
        values: Vec<Option<u16>>,
        n: usize,
        panic_at: Option<usize>,
    }
    impl AdcSample for FakeAdc {
        fn sample_mv(&mut self) -> Option<u16> {
            let i = self.n;
            self.n += 1;
            if self.panic_at == Some(i) {
                panic!("sampler panicked");
            }
            let v = self.values[i.min(self.values.len() - 1)];
            self.log.borrow_mut().push(match v {
                Some(_) => Ev::Sample(i as u8),
                None => Ev::SampleFail(i as u8),
            });
            v
        }
    }

    type Mon = BatteryMonitor<FakeCharge, FakeAdc, FakeDelay>;

    fn monitor(values: Vec<Option<u16>>) -> (Mon, Log) {
        monitor_with(values, None)
    }

    fn monitor_with(values: Vec<Option<u16>>, panic_at: Option<usize>) -> (Mon, Log) {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let m = BatteryMonitor::new(
            FakeCharge(log.clone()),
            FakeAdc {
                log: log.clone(),
                values,
                n: 0,
                panic_at,
            },
            FakeDelay(log.clone()),
        );
        (m, log)
    }

    fn ok_values(mv: u16) -> Vec<Option<u16>> {
        vec![Some(mv)]
    }

    fn charge_writes(log: &Log) -> Vec<bool> {
        log.borrow()
            .iter()
            .filter_map(|e| match e {
                Ev::Charge(b) => Some(*b),
                _ => None,
            })
            .collect()
    }

    // ---- R16: sequence ------------------------------------------------

    #[test]
    fn r16_sequence_is_enable_pause_settle_16_reads_resume() {
        let (mut m, log) = monitor(ok_values(1900));
        log.borrow_mut().clear(); // drop the constructor's enable
        m.measure().unwrap();
        let mut want = vec![Ev::Charge(false), Ev::Delay(30)];
        want.extend((0..16).map(Ev::Sample));
        want.push(Ev::Charge(true));
        assert_eq!(*log.borrow(), want);
    }

    #[test]
    fn r16_constructor_drives_charging_enabled_before_anything_else() {
        let (_m, log) = monitor(ok_values(1900));
        assert_eq!(*log.borrow(), vec![Ev::Charge(true)]);
    }

    #[test]
    fn r16_contract_constants_are_the_bsp_values() {
        assert_eq!(CHARGE_SETTLE_MS, 30); // board_c61.c:292
        assert_eq!(SAMPLE_COUNT, 16); // :295
        assert_eq!(DIVIDER_MULT, 2); // :303
        assert_eq!(BATTERY_ADC_CHANNEL, 3); // :63
    }

    #[test]
    fn r16_no_read_happens_before_the_settle_delay_or_while_charging() {
        let (mut m, log) = monitor(ok_values(1800));
        log.borrow_mut().clear();
        m.measure().unwrap();
        let l = log.borrow();
        let pause = l.iter().position(|e| *e == Ev::Charge(false)).unwrap();
        let settle = l.iter().position(|e| *e == Ev::Delay(30)).unwrap();
        let first = l.iter().position(|e| *e == Ev::Sample(0)).unwrap();
        let last = l.iter().position(|e| *e == Ev::Sample(15)).unwrap();
        let resume = l.iter().position(|e| *e == Ev::Charge(true)).unwrap();
        assert!(pause < settle && settle < first && last < resume);
    }

    #[test]
    fn r16_charge_pin_is_not_written_between_pause_and_resume() {
        let (mut m, log) = monitor(ok_values(1800));
        log.borrow_mut().clear();
        m.measure().unwrap();
        let l = log.borrow();
        let pause = l.iter().position(|e| *e == Ev::Charge(false)).unwrap();
        let resume = l.iter().rposition(|e| *e == Ev::Charge(true)).unwrap();
        assert!(
            l[pause + 1..resume]
                .iter()
                .all(|e| !matches!(e, Ev::Charge(_))),
            "{:?}",
            *l
        );
    }

    // ---- R16: failure paths ---------------------------------------------

    #[test]
    fn r16_adc_failure_at_every_index_stops_reading_and_still_resumes() {
        for k in 0..16u8 {
            let mut values: Vec<Option<u16>> = vec![Some(1900); 16];
            values[k as usize] = None;
            let (mut m, log) = monitor(values);
            log.borrow_mut().clear();
            assert_eq!(m.measure(), Err(BatteryError::AdcFailed { sample: k }));
            let l = log.borrow();
            // pause, settle, k good reads, the failing read, resume; nothing after
            let mut want = vec![Ev::Charge(false), Ev::Delay(30)];
            want.extend((0..k).map(Ev::Sample));
            want.push(Ev::SampleFail(k));
            want.push(Ev::Charge(true));
            assert_eq!(*l, want, "fail at {}", k);
        }
    }

    #[test]
    fn r16_adc_failure_is_an_error_not_zero_mv() {
        // every read fails: must not be Ok(0 mV) -> 0 % -> "empty battery"
        let (mut m, _log) = monitor(vec![None]);
        assert!(matches!(m.measure(), Err(BatteryError::AdcFailed { .. })));
    }

    #[test]
    fn r16_zero_mv_reads_are_a_valid_reading_not_an_error() {
        let (mut m, _log) = monitor(ok_values(0));
        let r = m.measure().unwrap();
        assert_eq!((r.adc_mv, r.cell_mv, r.percent()), (0, 0, 0));
    }

    #[test]
    fn r16_repeated_failures_leave_charging_enabled_and_monitor_reusable() {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let mut m = BatteryMonitor::new(
            FakeCharge(log.clone()),
            FailThenOk {
                log: log.clone(),
                fail_next: 5,
            },
            FakeDelay(log.clone()),
        );
        for _ in 0..5 {
            assert!(m.measure().is_err());
            assert_eq!(charge_writes(&log).last(), Some(&true));
        }
        let ok = m.measure().unwrap();
        assert_eq!(ok.adc_mv, 1900);
        let w = charge_writes(&log);
        // enable, then 6 x (pause, resume): strictly alternating, ends enabled
        assert_eq!(w.len(), 1 + 12);
        assert!(w[0]);
        for pair in w[1..].chunks(2) {
            assert_eq!(pair, [false, true]);
        }
    }

    /// Fails the next `fail_next` reads (each failing measurement stops at its
    /// first read, so 5 reads = 5 failed measurements), then succeeds.
    struct FailThenOk {
        log: Log,
        fail_next: u32,
    }
    impl AdcSample for FailThenOk {
        fn sample_mv(&mut self) -> Option<u16> {
            if self.fail_next > 0 {
                self.fail_next -= 1;
                self.log.borrow_mut().push(Ev::SampleFail(0));
                None
            } else {
                Some(1900)
            }
        }
    }

    #[test]
    fn r16_charging_is_resumed_even_if_the_sampler_panics() {
        // RAII: unwinding through measure() still runs the guard's Drop
        let (mut m, log) = monitor_with(ok_values(1900), Some(3));
        log.borrow_mut().clear();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| m.measure()));
        assert!(r.is_err());
        assert_eq!(charge_writes(&log), vec![false, true]);
    }

    // ---- R16: repeat calls / reentry -------------------------------------

    #[test]
    fn r16_repeated_calls_are_independent_complete_sequences() {
        let (mut m, log) = monitor(ok_values(1900));
        log.borrow_mut().clear();
        for _ in 0..3 {
            m.measure().unwrap();
        }
        let w = charge_writes(&log);
        assert_eq!(w, vec![false, true, false, true, false, true]);
        let delays = log
            .borrow()
            .iter()
            .filter(|e| matches!(e, Ev::Delay(_)))
            .count();
        assert_eq!(delays, 3, "one settle per measurement");
        let reads = log
            .borrow()
            .iter()
            .filter(|e| matches!(e, Ev::Sample(_)))
            .count();
        assert_eq!(reads, 48);
    }

    #[test]
    fn r16_pause_is_never_nested_across_a_failure_then_success_mix() {
        let (mut m, log) = monitor(vec![None]);
        let _ = m.measure();
        let _ = m.measure();
        let w = charge_writes(&log);
        // depth of "paused" never exceeds 1 and ends at 0
        let mut depth = 0i32;
        for e in &w[1..] {
            depth += if *e { -1 } else { 1 };
            assert!((0..=1).contains(&depth));
        }
        assert_eq!(depth, 0);
    }

    // ---- R16: conversion --------------------------------------------------

    #[test]
    fn r16_cell_voltage_is_twice_the_truncated_mean() {
        let (mut m, _l) = monitor(ok_values(1900));
        let r = m.measure().unwrap();
        assert_eq!((r.adc_mv, r.cell_mv), (1900, 3800));

        // mean truncates (1000.9375 -> 1000), then x2: rounding is floor
        let mut v = vec![Some(1000u16); 16];
        for x in v.iter_mut().take(15) {
            *x = Some(1001);
        }
        v[15] = Some(1000);
        let (mut m, _l) = monitor(v);
        let r = m.measure().unwrap();
        assert_eq!((r.adc_mv, r.cell_mv), (1000, 2000));
    }

    #[test]
    fn r16_mean_uses_all_16_reads() {
        // reads 0..16 -> sum 120 -> mean 7 (floor)
        let v: Vec<Option<u16>> = (0..16u16).map(Some).collect();
        let (mut m, _l) = monitor(v);
        assert_eq!(m.measure().unwrap().adc_mv, 7);
    }

    #[test]
    fn r16_sum_does_not_overflow_for_max_reads() {
        let (mut m, _l) = monitor(ok_values(u16::MAX));
        let r = m.measure().unwrap();
        assert_eq!(r.adc_mv, u16::MAX);
        assert_eq!(r.cell_mv, u16::MAX, "x2 saturates, never wraps");
    }

    #[test]
    fn r16_cell_mv_saturates_at_the_u16_edge() {
        assert_eq!(cell_mv_from_adc_mv(0), 0);
        assert_eq!(cell_mv_from_adc_mv(2100), 4200);
        assert_eq!(cell_mv_from_adc_mv(32767), 65534);
        assert_eq!(cell_mv_from_adc_mv(32768), u16::MAX);
        assert_eq!(cell_mv_from_adc_mv(u16::MAX), u16::MAX);
    }

    // ---- R16: voltage -> percent ------------------------------------------

    #[test]
    fn r16_percent_clamps_at_both_ends() {
        assert_eq!(percentage(0), 0);
        assert_eq!(percentage(2999), 0);
        assert_eq!(percentage(3000), 0);
        assert_eq!(percentage(4200), 100);
        assert_eq!(percentage(4201), 100);
        assert_eq!(percentage(u16::MAX), 100);
    }

    #[test]
    fn r16_percent_hits_every_knot_exactly() {
        for &(mv, pct) in LIPO_DISCHARGE_CURVE {
            assert_eq!(percentage(mv as u16), pct, "{} mV", mv);
        }
    }

    #[test]
    fn r16_percent_between_knots_floors() {
        // 3400 (5 %) .. 3600 (10 %): 5 % per 200 mV
        assert_eq!(percentage(3439), 5); // 39*5/200 = 0
        assert_eq!(percentage(3440), 6); // 40*5/200 = 1
        assert_eq!(percentage(3599), 9); // 199*5/200 = 4
        // 4060 (90 %) .. 4200 (100 %): 10 % per 140 mV
        assert_eq!(percentage(4130), 95); // 70*10/140 = 5
        assert_eq!(percentage(4199), 99);
        // lowest segment 3000 (0) .. 3400 (5): 5 % per 400 mV
        assert_eq!(percentage(3079), 0);
        assert_eq!(percentage(3080), 1);
    }

    #[test]
    fn r16_percent_is_monotonic_and_bounded_over_the_whole_range() {
        let mut prev = 0u8;
        for mv in 0..=u16::MAX {
            let p = percentage(mv);
            assert!(p >= prev && p <= 100, "{} mV -> {}", mv, p);
            prev = p;
        }
    }

    #[test]
    fn r16_percent_curve_is_descending_and_non_degenerate() {
        for w in LIPO_DISCHARGE_CURVE.windows(2) {
            assert!(w[0].0 > w[1].0 && w[0].1 > w[1].1);
        }
        assert_eq!(percentage_from_curve(&[], 4000), 0);
    }

    #[test]
    fn r16_reading_percent_uses_the_cell_voltage_not_the_pin_voltage() {
        // pin 1900 mV -> cell 3800 mV -> 40..50 %; if the x2 were forgotten
        // 1900 mV would read 0 %.
        let (mut m, _l) = monitor(ok_values(1900));
        let p = m.measure().unwrap().percent();
        assert!((40..50).contains(&p), "{}", p);
    }

    // ---- X4 equivalence of the moved code ----------------------------------

    /// The X4 table and algorithm exactly as they were in
    /// kernel/src/board/battery.rs and kernel/src/drivers/battery.rs (HEAD
    /// 3bb911af) before the move into this crate.
    mod x4_reference {
        pub const CURVE: &[(u32, u8)] = &[
            (4200, 100),
            (4060, 90),
            (3980, 80),
            (3920, 70),
            (3870, 60),
            (3830, 50),
            (3790, 40),
            (3750, 30),
            (3700, 20),
            (3600, 10),
            (3400, 5),
            (3000, 0),
        ];

        pub fn battery_percentage(battery_mv: u16) -> u8 {
            let mv = battery_mv as u32;
            if mv >= CURVE[0].0 {
                return CURVE[0].1;
            }
            let last = CURVE.len() - 1;
            if mv <= CURVE[last].0 {
                return CURVE[last].1;
            }
            let mut i = 0;
            while i + 1 < CURVE.len() {
                let (mv_hi, pct_hi) = CURVE[i];
                let (mv_lo, pct_lo) = CURVE[i + 1];
                if mv >= mv_lo {
                    let span_mv = mv_hi - mv_lo;
                    if span_mv == 0 {
                        return pct_hi;
                    }
                    let span_pct = (pct_hi - pct_lo) as u32;
                    let frac = mv - mv_lo;
                    return (pct_lo as u32 + frac * span_pct / span_mv) as u8;
                }
                i += 1;
            }
            0
        }
    }

    #[test]
    fn x4_curve_table_is_unchanged() {
        assert_eq!(LIPO_DISCHARGE_CURVE, x4_reference::CURVE);
    }

    #[test]
    fn x4_percentage_matches_the_pre_move_algorithm_for_every_u16() {
        for mv in 0..=u16::MAX {
            assert_eq!(
                percentage_from_curve(LIPO_DISCHARGE_CURVE, mv as u32),
                x4_reference::battery_percentage(mv),
                "{} mV",
                mv
            );
        }
    }
}
