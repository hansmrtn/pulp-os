// SD card detect, init flow and storage-error state (R9, plus the R6/R7
// hand-off with the GPIO27 power state machine).
//
// Hardware facts (../bsp_onepage_c61/board_c61.c):
//   * card detect is GPIO28, input with internal pull-up, "mechanical CD
//     switch" (:59, :282-285);
//   * `board_sd_present()` is `gpio_get_level(PIN_SD_CD) == 0` with the BSP's
//     own comment "assume low = inserted" (:419). The BSP itself marks the
//     polarity as an assumption, so it is UNVERIFIED on hardware, and this
//     module therefore never uses card detect to *block* an init attempt: it
//     only classifies a failure (no card vs. card present but unusable) and
//     emits insert/remove events for re-mount.
//
// The kernel (board_c61) implements `SdProbe` over embedded-sdmmc and calls
// `init_sd`. Init requires an `SdInitPermit`, which only
// `PeripheralPower::begin_sd_init` mints (after the GPIO27 power-cycle).

use crate::power::{DelayMs, PeripheralPower, PowerError, RailPin, RailState, SdInitPermit};

/// Card-detect level that means "card inserted". board_c61.c:419
/// (`== 0`, "assume low = inserted"); pull-up enabled at :284.
pub const CD_PRESENT_WHEN_LOW: bool = true;
/// Consecutive differing samples needed to accept a card-detect change.
/// Not in the BSP (it only samples on demand); mechanical switches bounce.
pub const CD_DEBOUNCE_SAMPLES: u8 = 3;
/// Polling period for `CardDetect::sample` (debounce window = N x this).
/// Not in the BSP; chosen so a switch bounce (a few ms) is filtered.
pub const CD_SAMPLE_INTERVAL_MS: u32 = 20;
/// SD init attempts and spacing: same policy as the X4 `SdStorage::init_card`
/// (5 attempts, 50 ms apart).
pub const SD_INIT_ATTEMPTS: u8 = 5;
pub const SD_INIT_RETRY_DELAY_MS: u32 = 50;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CardState {
    Absent,
    Present,
}

/// Map the raw GPIO28 level to presence using the BSP polarity.
pub const fn card_state_from_level(level_high: bool) -> CardState {
    if level_high == CD_PRESENT_WHEN_LOW {
        CardState::Absent
    } else {
        CardState::Present
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CardEvent {
    Inserted,
    Removed,
}

/// Debouncer for the card-detect switch. Feed one raw sample per poll; a
/// change is reported only after `CD_DEBOUNCE_SAMPLES` consecutive samples
/// disagree with the current stable state.
#[derive(Copy, Clone, Debug)]
pub struct CardDetect {
    stable: CardState,
    pending: u8,
}

impl CardDetect {
    pub const fn new(initial: CardState) -> Self {
        Self {
            stable: initial,
            pending: 0,
        }
    }

    pub const fn state(&self) -> CardState {
        self.stable
    }

    /// `level_high`: raw GPIO28 level.
    pub fn sample(&mut self, level_high: bool) -> Option<CardEvent> {
        let observed = card_state_from_level(level_high);
        if observed == self.stable {
            self.pending = 0;
            return None;
        }
        self.pending += 1;
        if self.pending < CD_DEBOUNCE_SAMPLES {
            return None;
        }
        self.pending = 0;
        self.stable = observed;
        Some(match observed {
            CardState::Present => CardEvent::Inserted,
            CardState::Absent => CardEvent::Removed,
        })
    }
}

/// Everything that can go wrong on the SD path. All variants are recoverable:
/// no code path turns them into a panic.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SdFault {
    /// Init failed and card detect says nothing is inserted.
    NoCard,
    /// Init failed with a card present (or card detect disagrees).
    InitFailed,
    /// Card answered but the FAT volume/root could not be opened.
    MountFailed,
    ReadFailed,
    WriteFailed,
    /// SD init was requested while GPIO27 was not in a state that allows it
    /// (power-cycle missing or init already running). Nothing was touched.
    PowerNotReady(RailState),
}

/// What init failure means given the card-detect reading.
pub const fn classify_init_failure(card: CardState) -> SdFault {
    match card {
        CardState::Absent => SdFault::NoCard,
        CardState::Present => SdFault::InitFailed,
    }
}

/// Card driver seen by the init flow (embedded-sdmmc `SdCard` in the kernel).
pub trait SdProbe {
    /// One init + capacity query. `None` = the card did not answer properly.
    fn probe(&mut self) -> Option<u64>;
    /// Forget partial init state before the next attempt.
    fn mark_uninit(&mut self);
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SdInitReport {
    pub bytes: u64,
    /// 1-based attempt that succeeded.
    pub attempts: u8,
}

/// Retry loop. Requires the permit by reference: SD traffic can only start
/// after the GPIO27 power-cycle (type-level R6).
pub fn probe_with_retry<P: SdProbe, D: DelayMs>(
    _permit: &SdInitPermit,
    probe: &mut P,
    delay: &mut D,
) -> Option<SdInitReport> {
    for attempt in 1..=SD_INIT_ATTEMPTS {
        match probe.probe() {
            Some(bytes) => {
                return Some(SdInitReport {
                    bytes,
                    attempts: attempt,
                });
            }
            None => {
                probe.mark_uninit();
                delay.delay_ms(SD_INIT_RETRY_DELAY_MS);
            }
        }
    }
    None
}

/// Full SD init: take the permit, probe with retry, report the result to the
/// power state machine. The card-detect reading is only used to classify a
/// failure (see module comment), never to skip the attempt.
pub fn init_sd<R: RailPin, P: SdProbe, D: DelayMs>(
    power: &mut PeripheralPower<R>,
    card: CardState,
    probe: &mut P,
    delay: &mut D,
) -> Result<SdInitReport, SdFault> {
    let permit = power.begin_sd_init().map_err(|e| match e {
        PowerError::InvalidState(s) => SdFault::PowerNotReady(s),
        PowerError::HardwareResetForbidden => SdFault::PowerNotReady(power.state()),
    })?;
    let result = probe_with_retry(&permit, probe, delay);
    power.finish_sd_init(permit, result.is_some());
    result.ok_or(classify_init_failure(card))
}

/// Storage state that the UI can show ("SD error").
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StorageStatus {
    /// Init not attempted yet, or a re-insert is waiting for re-mount.
    Uninitialised,
    Ready,
    NoCard,
    InitFailed,
    MountFailed,
    /// A read or write failed while mounted.
    IoError,
}

impl StorageStatus {
    pub const fn is_error(self) -> bool {
        !matches!(self, Self::Ready | Self::Uninitialised)
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::Uninitialised => "SD: not ready",
            Self::Ready => "SD: ok",
            Self::NoCard => "SD: no card",
            Self::InitFailed => "SD: init failed",
            Self::MountFailed => "SD: mount failed",
            Self::IoError => "SD: read/write error",
        }
    }
}

impl From<SdFault> for StorageStatus {
    fn from(f: SdFault) -> Self {
        match f {
            SdFault::NoCard => Self::NoCard,
            SdFault::InitFailed | SdFault::PowerNotReady(_) => Self::InitFailed,
            SdFault::MountFailed => Self::MountFailed,
            SdFault::ReadFailed | SdFault::WriteFailed => Self::IoError,
        }
    }
}

/// Tracks storage health across init, runtime I/O and card insert/remove.
/// Every transition is recoverable; there is no terminal error state.
#[derive(Copy, Clone, Debug)]
pub struct StorageHealth {
    status: StorageStatus,
}

impl Default for StorageHealth {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageHealth {
    pub const fn new() -> Self {
        Self {
            status: StorageStatus::Uninitialised,
        }
    }

    pub const fn status(&self) -> StorageStatus {
        self.status
    }

    /// Init, mount or I/O succeeded.
    pub fn on_success(&mut self) {
        self.status = StorageStatus::Ready;
    }

    /// Init, mount or I/O failed.
    pub fn on_fault(&mut self, fault: SdFault) {
        self.status = fault.into();
    }

    /// Card-detect event. Returns true when the caller should re-run init +
    /// mount (a card appeared while storage is not usable). Removal always
    /// drops to `NoCard`; the caller must also drop the mounted volume and
    /// call `PeripheralPower::card_removed`.
    pub fn on_card_event(&mut self, ev: CardEvent) -> bool {
        match ev {
            CardEvent::Removed => {
                self.status = StorageStatus::NoCard;
                false
            }
            CardEvent::Inserted => {
                if self.status == StorageStatus::Ready {
                    false
                } else {
                    self.status = StorageStatus::Uninitialised;
                    true
                }
            }
        }
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
        PinLow,
        PinHigh,
        Delay(u32),
        Probe,
        MarkUninit,
    }
    type Log = Rc<RefCell<Vec<Ev>>>;

    struct FakePin(Log);
    impl RailPin for FakePin {
        fn set_high(&mut self) {
            self.0.borrow_mut().push(Ev::PinHigh);
        }
        fn set_low(&mut self) {
            self.0.borrow_mut().push(Ev::PinLow);
        }
    }
    struct FakeDelay(Log);
    impl DelayMs for FakeDelay {
        fn delay_ms(&mut self, ms: u32) {
            self.0.borrow_mut().push(Ev::Delay(ms));
        }
    }
    /// answers `None` for the first `fail_first` probes, then `Some(bytes)`
    /// (`fail_first = u32::MAX`: never answers)
    struct FakeCard {
        log: Log,
        fail_first: u32,
        calls: u32,
    }
    impl SdProbe for FakeCard {
        fn probe(&mut self) -> Option<u64> {
            self.log.borrow_mut().push(Ev::Probe);
            self.calls += 1;
            if self.calls > self.fail_first {
                Some(8 * 1024 * 1024 * 1024)
            } else {
                None
            }
        }
        fn mark_uninit(&mut self) {
            self.log.borrow_mut().push(Ev::MarkUninit);
        }
    }

    fn rig(fail_first: u32) -> (PeripheralPower<FakePin>, FakeDelay, FakeCard, Log) {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        (
            PeripheralPower::new(FakePin(log.clone())),
            FakeDelay(log.clone()),
            FakeCard {
                log: log.clone(),
                fail_first,
                calls: 0,
            },
            log,
        )
    }

    // ---- card detect (R9) ------------------------------------------------

    #[test]
    fn r9_cd_polarity_is_bsp_low_means_inserted() {
        assert!(CD_PRESENT_WHEN_LOW);
        assert_eq!(card_state_from_level(false), CardState::Present);
        assert_eq!(card_state_from_level(true), CardState::Absent);
        assert_eq!(crate::pins::SD_CARD_DETECT, 28);
    }

    #[test]
    fn r9_cd_change_needs_n_consecutive_samples() {
        let mut cd = CardDetect::new(CardState::Absent);
        // N-1 samples of "present" (low): still absent, no event
        for _ in 0..CD_DEBOUNCE_SAMPLES - 1 {
            assert_eq!(cd.sample(false), None);
            assert_eq!(cd.state(), CardState::Absent);
        }
        assert_eq!(cd.sample(false), Some(CardEvent::Inserted));
        assert_eq!(cd.state(), CardState::Present);
        // steady state emits nothing
        assert_eq!(cd.sample(false), None);
    }

    #[test]
    fn r9_cd_bounce_is_filtered() {
        let mut cd = CardDetect::new(CardState::Present);
        // contact bounce: never N consecutive "absent" samples
        for _ in 0..10 {
            assert_eq!(cd.sample(true), None); // absent
            assert_eq!(cd.sample(true), None); // absent
            assert_eq!(cd.sample(false), None); // back to present resets
        }
        assert_eq!(cd.state(), CardState::Present);
    }

    #[test]
    fn r9_cd_removal_event_after_debounce() {
        let mut cd = CardDetect::new(CardState::Present);
        let evs: Vec<_> = (0..CD_DEBOUNCE_SAMPLES).map(|_| cd.sample(true)).collect();
        assert_eq!(evs.last(), Some(&Some(CardEvent::Removed)));
        assert_eq!(evs.iter().flatten().count(), 1);
        assert_eq!(cd.state(), CardState::Absent);
    }

    // ---- init flow (R6 permit, R9 errors) --------------------------------

    #[test]
    fn r6_init_sd_is_refused_before_power_cycle_and_touches_nothing() {
        let (mut p, mut d, mut c, log) = rig(0);
        let r = init_sd(&mut p, CardState::Present, &mut c, &mut d);
        assert_eq!(r, Err(SdFault::PowerNotReady(RailState::Unpowered)));
        assert!(log.borrow().is_empty(), "no pin, delay or probe event");
        assert_eq!(p.state(), RailState::Unpowered);
    }

    #[test]
    fn r6_probe_runs_only_after_the_power_cycle_events() {
        let (mut p, mut d, mut c, log) = rig(0);
        p.power_cycle(&mut d).unwrap();
        let r = init_sd(&mut p, CardState::Present, &mut c, &mut d).unwrap();
        assert_eq!(r.attempts, 1);
        assert_eq!(
            *log.borrow(),
            vec![
                Ev::PinLow,
                Ev::Delay(20),
                Ev::PinHigh,
                Ev::Delay(20),
                Ev::Probe
            ]
        );
        assert_eq!(p.state(), RailState::SdActive);
    }

    #[test]
    fn r9_init_retries_with_50ms_spacing_then_succeeds() {
        let (mut p, mut d, mut c, log) = rig(2);
        p.power_cycle(&mut d).unwrap();
        log.borrow_mut().clear();
        let r = init_sd(&mut p, CardState::Present, &mut c, &mut d).unwrap();
        assert_eq!(r.attempts, 3);
        assert_eq!(
            *log.borrow(),
            vec![
                Ev::Probe,
                Ev::MarkUninit,
                Ev::Delay(50),
                Ev::Probe,
                Ev::MarkUninit,
                Ev::Delay(50),
                Ev::Probe
            ]
        );
    }

    #[test]
    fn r9_no_card_is_a_recoverable_error_not_a_panic() {
        let (mut p, mut d, mut c, log) = rig(u32::MAX);
        p.power_cycle(&mut d).unwrap();
        let before_pin: usize = log
            .borrow()
            .iter()
            .filter(|e| matches!(e, Ev::PinLow | Ev::PinHigh))
            .count();
        let r = init_sd(&mut p, CardState::Absent, &mut c, &mut d);
        assert_eq!(r, Err(SdFault::NoCard));
        let probes = log.borrow().iter().filter(|e| **e == Ev::Probe).count();
        assert_eq!(probes, SD_INIT_ATTEMPTS as usize, "bounded retries");
        // rail untouched, state back to PowerCycled so a later insert retries
        let after_pin: usize = log
            .borrow()
            .iter()
            .filter(|e| matches!(e, Ev::PinLow | Ev::PinHigh))
            .count();
        assert_eq!(before_pin, after_pin);
        assert_eq!(p.state(), RailState::PowerCycled);
        // and the retry is actually possible
        c.fail_first = 0;
        c.calls = 0;
        assert!(init_sd(&mut p, CardState::Present, &mut c, &mut d).is_ok());
    }

    #[test]
    fn r9_card_present_but_unusable_is_init_failed() {
        let (mut p, mut d, mut c, _log) = rig(u32::MAX);
        p.power_cycle(&mut d).unwrap();
        assert_eq!(
            init_sd(&mut p, CardState::Present, &mut c, &mut d),
            Err(SdFault::InitFailed)
        );
    }

    #[test]
    fn r9_card_detect_absent_does_not_block_an_init_attempt() {
        // polarity is unverified on hardware: a card that answers must be
        // usable even if GPIO28 reads "absent"
        let (mut p, mut d, mut c, _log) = rig(0);
        p.power_cycle(&mut d).unwrap();
        assert!(init_sd(&mut p, CardState::Absent, &mut c, &mut d).is_ok());
        assert_eq!(p.state(), RailState::SdActive);
    }

    #[test]
    fn r9_second_init_while_active_is_refused_not_panicking() {
        let (mut p, mut d, mut c, _log) = rig(0);
        p.power_cycle(&mut d).unwrap();
        init_sd(&mut p, CardState::Present, &mut c, &mut d).unwrap();
        assert_eq!(
            init_sd(&mut p, CardState::Present, &mut c, &mut d),
            Err(SdFault::PowerNotReady(RailState::SdActive))
        );
    }

    #[test]
    fn r9_remove_then_reinsert_remounts_through_a_new_permit() {
        let (mut p, mut d, mut c, log) = rig(0);
        p.power_cycle(&mut d).unwrap();
        init_sd(&mut p, CardState::Present, &mut c, &mut d).unwrap();
        let pins_before = log
            .borrow()
            .iter()
            .filter(|e| matches!(e, Ev::PinLow | Ev::PinHigh))
            .count();

        let mut h = StorageHealth::new();
        h.on_success();
        assert!(!h.on_card_event(CardEvent::Removed));
        assert_eq!(h.status(), StorageStatus::NoCard);
        p.card_removed().unwrap();

        assert!(
            h.on_card_event(CardEvent::Inserted),
            "insert asks for re-init"
        );
        assert_eq!(h.status(), StorageStatus::Uninitialised);
        c.calls = 0;
        init_sd(&mut p, CardState::Present, &mut c, &mut d).unwrap();
        h.on_success();
        assert_eq!(h.status(), StorageStatus::Ready);

        let pins_after = log
            .borrow()
            .iter()
            .filter(|e| matches!(e, Ev::PinLow | Ev::PinHigh))
            .count();
        assert_eq!(
            pins_before, pins_after,
            "GPIO27 untouched across remove/insert"
        );
    }

    // ---- storage status (R9 "show a recoverable storage error") ----------

    #[test]
    fn r9_every_fault_maps_to_a_displayable_error_status() {
        let cases = [
            (SdFault::NoCard, StorageStatus::NoCard),
            (SdFault::InitFailed, StorageStatus::InitFailed),
            (SdFault::MountFailed, StorageStatus::MountFailed),
            (SdFault::ReadFailed, StorageStatus::IoError),
            (SdFault::WriteFailed, StorageStatus::IoError),
            (
                SdFault::PowerNotReady(RailState::Unpowered),
                StorageStatus::InitFailed,
            ),
        ];
        for (fault, want) in cases {
            let mut h = StorageHealth::new();
            h.on_fault(fault);
            assert_eq!(h.status(), want);
            assert!(h.status().is_error());
            assert!(h.status().message().starts_with("SD: "));
        }
        assert!(!StorageStatus::Ready.is_error());
        assert!(!StorageStatus::Uninitialised.is_error());
    }

    #[test]
    fn r9_io_error_recovers_on_next_success() {
        let mut h = StorageHealth::new();
        h.on_success();
        h.on_fault(SdFault::ReadFailed);
        assert_eq!(h.status(), StorageStatus::IoError);
        h.on_success();
        assert_eq!(h.status(), StorageStatus::Ready);
    }

    #[test]
    fn r9_insert_while_ready_does_not_trigger_remount() {
        let mut h = StorageHealth::new();
        h.on_success();
        assert!(!h.on_card_event(CardEvent::Inserted));
        assert_eq!(h.status(), StorageStatus::Ready);
    }

    #[test]
    fn r9_insert_after_failed_init_triggers_remount() {
        let mut h = StorageHealth::new();
        h.on_fault(SdFault::NoCard);
        assert!(h.on_card_event(CardEvent::Inserted));
        assert_eq!(h.status(), StorageStatus::Uninitialised);
    }
}
