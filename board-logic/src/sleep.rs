// Deep-sleep entry contract (R18, R19, R20), HAL-free.
//
// One function, `SleepSequence::enter_deep_sleep`, owns the order of everything
// that happens between "the app wants to sleep" and "the chip sleeps". The
// firmware (`kernel::board_c61::sleep`) and the host tests drive this very
// code; hardware is reached only through the small traits below.
//
// BSP reference (../bsp_onepage_c61/board_c61.c, `board_sleep_enter` :335-359):
//
//   :337 board_display_sleep()            park the EPD controller first
//   :340-344 gpio_set_level(SCLK, MOSI, EPD CS, SD CS, PDM CLK) = 0
//                                         silence shared lines so they cannot
//                                         back-power the cut-off rail
//   :346 board_peripherals_power(false)   GPIO27 low = SD/MIC/EPD power cut
//   :348-352 esp_sleep_enable_gpio_wakeup_on_hp_periph_powerdown(GPIO2, LOW)
//   :353-354 timer wake (not used here)
//   :356 esp_deep_sleep_start()
//
// Sequence implemented here (each line is a trace event in the host tests):
//
//   0. rail precheck   only `PowerCycled`/`SdActive` may start; anything else
//                      aborts before ANY side effect
//   1. save session    R18: first, while SD is active and GPIO27 is high
//   2. arm GPIO2 wake  low level, pull-up; failure ABORTS here (see below)
//   3. begin_shutdown  no new SD init / reset / save from now on
//   4. park EPD        BSP :337  (DEEP_SLEEP mode 1)
//   5. SD shutdown     flush FAT handles (BSP has no SD step: it cuts power)
//   6. charge restore  GPIO10 = charging allowed (BSP has no such step)
//   7. silence lines   BSP :340-344
//   8. cut GPIO27      BSP :346
//   9. enter deep sleep BSP :356, only with a `PoweredOffProof` + `WakeArmed`
//
// DEVIATION from the BSP order, deliberate: the BSP arms the wake source
// (:348-352) AFTER cutting GPIO27. Arming has no electrical dependency on the
// rail (GPIO2 is an LP pad with its own pull-up and is not on the GPIO27
// supply), so it is moved to step 2. Reason: wake arming is the one step whose
// failure leaves the device unrecoverable (esp-hal panics in `sleep_deep` when
// no wake source is enabled; a chip with the rail cut and no wake source only
// comes back by reset), and every step from 3 on is irreversible (the EPD in
// deep sleep mode 1 needs a hardware reset to wake, which on this board is the
// GPIO27 power-up the state machine forbids at runtime). Arming first makes
// "wake could not be configured" a clean abort with the device still fully
// usable. All BSP orderings that matter electrically are kept: park -> silence
// -> cut -> sleep, and the wake is armed before the cut and before sleep.
//
// Failure policy (each is tested):
//   * save failed or refused (card removed, SD error): reported, sleep goes on.
//     Not sleeping would keep the CPU awake until the battery dies; losing the
//     position only costs the user a page, and the previous slot is intact.
//   * EPD park / SD shutdown / charge restore / line silencing failed: recorded,
//     the sequence continues; each later step does not depend on an earlier one
//     having worked, and stopping half-way would leave a half powered board.
//   * wake arming failed, or the rail is in a state that cannot shut down:
//     abort (`SleepAbort`), nothing irreversible has happened, caller keeps
//     running. The saved position (if any) stays valid on the card.
//
// Not verified on hardware: everything physical (sleep current, whether GPIO2
// wakes the chip, pad state of GPIO27/GPIO10 during deep sleep, EPD image).

use crate::battery::{BatteryMonitor, ChargePin};
use crate::keys::AdcSample;
use crate::pins::KEY_WAKE;
use crate::power::{DelayMs, PeripheralPower, PowerError, PoweredOffProof, RailPin, RailState};
use crate::session::{
    BootDecision, NormalBootReason, Restored, SaveError, SaveReport, SessionState, SessionStore,
};

// ---------------------------------------------------------------------------
// wake source definition (BSP)
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WakeLevel {
    Low,
    High,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WakePull {
    Up,
    Down,
    None,
}

/// Which pad wakes the chip, on which level, and the resistor that holds it at
/// the opposite level while asleep (a floating wake pad wakes the chip at once,
/// see esp-hal `WakeupConfig`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WakeSpec {
    pub gpio: u8,
    pub level: WakeLevel,
    pub pull: WakePull,
}

/// GPIO2, wake on LOW, internal pull-up. BSP: `PIN_KEY_WAKE = GPIO_NUM_2`
/// "LP-capable deep-sleep wake key (active-low)" (board_c61.c:66),
/// `ESP_GPIO_WAKEUP_GPIO_LOW` (:352), side keys are `.active_level = 0` with
/// the internal pull-up enabled (board_keys.c:97-99).
pub const WAKE_SPEC: WakeSpec = WakeSpec {
    gpio: KEY_WAKE,
    level: WakeLevel::Low,
    pull: WakePull::Up,
};

/// Proof that the wake source was armed in this sequence. Only
/// `SleepSequence::enter_deep_sleep` creates one (private field), after
/// `WakeConfig::arm` returned `Ok`, and `SleepEntry::enter` demands it, so the
/// chip cannot be put to sleep without a wake source.
#[must_use = "pass the proof to the deep-sleep entry"]
#[derive(Debug)]
pub struct WakeArmed {
    spec: WakeSpec,
}

impl WakeArmed {
    pub fn spec(&self) -> WakeSpec {
        self.spec
    }
}

// ---------------------------------------------------------------------------
// collaborators
// ---------------------------------------------------------------------------

/// A step that failed; the text is for the log.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StepError(pub &'static str);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WakeError {
    /// The pad has no low-power wake path on this chip.
    NoLowPowerPath,
    /// The wake level is already present on the pad (the key is held down).
    /// Sleeping now would wake at once or sleep through the event (esp-hal
    /// `sleep_deep` has no rejection); abort and retry after the key is up.
    AlreadyAsserted,
    Other(&'static str),
}

/// Step 1. Saves the position while `power` is `SdActive`. The kernel adapter
/// is `StoreSaver` over the SD store; tests use fakes.
pub trait SessionSaver {
    fn save<P: RailPin>(&mut self, power: &PeripheralPower<P>) -> Result<SaveReport, SaveError>;
}

/// `SessionSaver` over any `SessionStore` (the SD store in the kernel): calls
/// the T10 `save_session`, which needs `SdActive` and refuses otherwise.
pub struct StoreSaver<'a, S: SessionStore> {
    pub store: &'a mut S,
    pub state: &'a SessionState,
}

impl<S: SessionStore> SessionSaver for StoreSaver<'_, S> {
    fn save<P: RailPin>(&mut self, power: &PeripheralPower<P>) -> Result<SaveReport, SaveError> {
        crate::session::save_session(power, self.store, self.state)
    }
}

/// Step 4: EPD deep sleep (BSP `board_display_sleep`, :337).
pub trait DisplayPark {
    fn park(&mut self) -> Result<(), StepError>;
}

/// Step 5: stop SD activity before the rail goes (flush and close FAT
/// handles). No CMD0 here, unlike X4: the card loses power right after.
pub trait SdShutdown {
    fn shutdown(&mut self) -> Result<(), StepError>;
}

/// Step 6: leave GPIO10 at "charging allowed".
pub trait ChargeRestore {
    fn restore(&mut self) -> Result<(), StepError>;
}

/// Step 7: drive SCK, MOSI, EPD CS, SD CS and PDM CLK low (BSP :340-344).
pub trait LineSilencer {
    fn silence(&mut self) -> Result<(), StepError>;
}

/// Step 2: make `spec` end deep sleep. Must be idempotent (arming twice
/// leaves the same configuration). Must fail with `WakeError::AlreadyAsserted`
/// when the wake level is already present on the pad.
pub trait WakeConfig {
    fn arm(&mut self, spec: &WakeSpec) -> Result<(), WakeError>;
}

/// Step 9. `Entered` is `core::convert::Infallible` in the firmware (the call
/// does not return) and `()` in tests.
pub trait SleepEntry {
    type Entered;
    fn enter(
        self,
        rail_off: PoweredOffProof<'_>,
        wake: WakeArmed,
        report: &SleepReport,
    ) -> Self::Entered;
}

impl<T: ChargeRestore + ?Sized> ChargeRestore for &mut T {
    fn restore(&mut self) -> Result<(), StepError> {
        (**self).restore()
    }
}

impl<T: ChargeRestore> ChargeRestore for Option<T> {
    fn restore(&mut self) -> Result<(), StepError> {
        match self {
            Some(t) => t.restore(),
            None => Err(StepError("no charge control (battery monitor absent)")),
        }
    }
}

impl<C: ChargePin, A: AdcSample, D: DelayMs> ChargeRestore for BatteryMonitor<C, A, D> {
    fn restore(&mut self) -> Result<(), StepError> {
        self.ensure_charging();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// the sequence
// ---------------------------------------------------------------------------

/// What happened on the way to sleep. Every step records its result.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SleepReport {
    pub save: Result<SaveReport, SaveError>,
    pub display: Result<(), StepError>,
    pub sd: Result<(), StepError>,
    pub charge: Result<(), StepError>,
    pub lines: Result<(), StepError>,
}

impl SleepReport {
    /// A position was written (and read back) before the power went.
    pub fn saved(&self) -> bool {
        self.save.is_ok()
    }

    pub fn all_steps_ok(&self) -> bool {
        self.save.is_ok()
            && self.display.is_ok()
            && self.sd.is_ok()
            && self.charge.is_ok()
            && self.lines.is_ok()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AbortReason {
    /// The rail cannot shut down from its current state.
    Rail(PowerError),
    /// The wake source could not be armed: sleeping would be one way.
    Wake(WakeError),
}

/// The sequence stopped before anything irreversible happened; the device is
/// still running and fully usable.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SleepAbort {
    pub reason: AbortReason,
    /// `None` when the abort came before the save step.
    pub save: Option<Result<SaveReport, SaveError>>,
}

pub struct SleepSequence<'a, P, Sv, Dp, Sd, Ch, Ln, Wk, En>
where
    P: RailPin,
    Sv: SessionSaver,
    Dp: DisplayPark,
    Sd: SdShutdown,
    Ch: ChargeRestore,
    Ln: LineSilencer,
    Wk: WakeConfig,
    En: SleepEntry,
{
    /// GPIO27 state machine (the "power rail"; there is no second path to it).
    pub power: &'a mut PeripheralPower<P>,
    pub saver: Sv,
    pub display: Dp,
    pub sd: Sd,
    pub charge: Ch,
    pub lines: Ln,
    pub wake: Wk,
    pub entry: En,
}

impl<P, Sv, Dp, Sd, Ch, Ln, Wk, En> SleepSequence<'_, P, Sv, Dp, Sd, Ch, Ln, Wk, En>
where
    P: RailPin,
    Sv: SessionSaver,
    Dp: DisplayPark,
    Sd: SdShutdown,
    Ch: ChargeRestore,
    Ln: LineSilencer,
    Wk: WakeConfig,
    En: SleepEntry,
{
    /// Run the whole sequence (module comment). `Ok` carries the report and what
    /// the entry returned (never, on hardware); `Err` means "stayed awake".
    pub fn enter_deep_sleep(mut self) -> Result<(SleepReport, En::Entered), SleepAbort> {
        // 0. precheck: refuse before touching anything
        match self.power.state() {
            RailState::PowerCycled | RailState::SdActive => {}
            s => {
                return Err(SleepAbort {
                    reason: AbortReason::Rail(PowerError::InvalidState(s)),
                    save: None,
                });
            }
        }

        // 1. R18: save first, while SD is active and GPIO27 still high
        let save = self.saver.save(&*self.power);

        // 2. wake source, before anything irreversible (see module comment)
        if let Err(e) = self.wake.arm(&WAKE_SPEC) {
            return Err(SleepAbort {
                reason: AbortReason::Wake(e),
                save: Some(save),
            });
        }
        let armed = WakeArmed { spec: WAKE_SPEC };

        // 3. no new SD init / reset / save from here on
        if let Err(e) = self.power.begin_shutdown() {
            return Err(SleepAbort {
                reason: AbortReason::Rail(e),
                save: Some(save),
            });
        }

        // 4-7. peripheral wind-down, each result recorded, none stops the rest
        let display = self.display.park();
        let sd = self.sd.shutdown();
        let charge = self.charge.restore();
        let lines = self.lines.silence();

        // 8. GPIO27 low. Cannot fail in `ShuttingDown`; mapped anyway.
        if let Err(e) = self.power.cut_peripheral_power() {
            return Err(SleepAbort {
                reason: AbortReason::Rail(e),
                save: Some(save),
            });
        }

        let report = SleepReport {
            save,
            display,
            sd,
            charge,
            lines,
        };

        // 9. sleep needs both proofs
        let off = match self.power.powered_off() {
            Ok(p) => p,
            Err(e) => {
                return Err(SleepAbort {
                    reason: AbortReason::Rail(e),
                    save: Some(report.save),
                });
            }
        };
        let entered = self.entry.enter(off, armed, &report);
        Ok((report, entered))
    }
}

// ---------------------------------------------------------------------------
// wake cause and boot decision (R20)
// ---------------------------------------------------------------------------

/// Why the chip started. Mirrors BSP `board_wake_cause_t`
/// (`BOARD_WAKE_POWERON/BY_KEY/BY_TIMER/OTHER`, board_c61.c:361-380).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WakeCause {
    /// Fresh boot / non-sleep reset.
    PowerOn,
    /// The GPIO2 wake key ended a deep sleep.
    Key,
    Timer,
    Other,
}

/// Wake sources as the HAL reports them, reduced to what the BSP
/// distinguishes. `key_path`: GPIO wake (esp-hal reports the low-power pad path
/// of the ESP32-C61 as `Ext1`, a digital pad wake as `Gpio`; both are the key).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct WakeBits {
    pub key_path: bool,
    pub timer: bool,
    /// Any other source (UART, SDIO, Wi-Fi, ...).
    pub other: bool,
}

/// Same priority as the BSP: key, then timer, then "no cause = power on",
/// otherwise other. `bits` must be empty for a reset that was not a deep-sleep
/// wake (esp-hal `wakeup_cause` returns an empty set then).
pub fn classify_wake(bits: WakeBits) -> WakeCause {
    if bits.key_path {
        WakeCause::Key
    } else if bits.timer {
        WakeCause::Timer
    } else if !bits.other {
        WakeCause::PowerOn
    } else {
        WakeCause::Other
    }
}

/// What to do after SD init (R20).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BootPlan {
    /// Reopen the saved reading position.
    Restore {
        cause: WakeCause,
        restored: Restored,
    },
    /// Boot to Home.
    Normal {
        cause: WakeCause,
        reason: NormalBootReason,
    },
}

impl BootPlan {
    pub fn cause(&self) -> WakeCause {
        match self {
            BootPlan::Restore { cause, .. } | BootPlan::Normal { cause, .. } => *cause,
        }
    }

    pub fn is_restore(&self) -> bool {
        matches!(self, BootPlan::Restore { .. })
    }
}

/// R20: "restart with valid persistent state -> restore the position". The
/// decision depends on the session only, not on the wake cause: the session
/// lives on the SD card, so unlike the X4 (RTC memory, zeroed on power-up) it
/// also survives a cold boot, and R20 asks for a restore on any restart. The
/// cause is carried for logging. An invalid or missing session is a normal boot
/// for every cause (T10: never an error, never a panic). T12 may add
/// `clear_session` after a restore to avoid boot loops on a bad position.
pub fn plan_boot(cause: WakeCause, decision: BootDecision) -> BootPlan {
    match decision {
        BootDecision::Restore(restored) => BootPlan::Restore { cause, restored },
        BootDecision::NormalBoot(reason) => BootPlan::Normal { cause, reason },
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::power::SdInitPermit;
    use crate::session::{DecodeError, Slot, StoreError, restore_session};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::vec;
    use std::vec::Vec;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Ev {
        /// saver called; the rail state it saw
        Save(RailState),
        Wake(WakeSpec),
        Park,
        SdStop,
        Charge,
        Silence,
        Gpio27Low,
        Gpio27High,
        Enter(WakeSpec),
    }

    type Log = Rc<RefCell<Vec<Ev>>>;

    struct Pin(Log);
    impl RailPin for Pin {
        fn set_high(&mut self) {
            self.0.borrow_mut().push(Ev::Gpio27High);
        }
        fn set_low(&mut self) {
            self.0.borrow_mut().push(Ev::Gpio27Low);
        }
    }
    struct NoDelay;
    impl DelayMs for NoDelay {
        fn delay_ms(&mut self, _ms: u32) {}
    }

    #[derive(Clone, Copy, Default)]
    struct Fail {
        save: bool,
        park: bool,
        sd: bool,
        charge: bool,
        lines: bool,
        wake: bool,
    }

    struct FakeSaver(Log, bool);
    impl SessionSaver for FakeSaver {
        fn save<P: RailPin>(
            &mut self,
            power: &PeripheralPower<P>,
        ) -> Result<SaveReport, SaveError> {
            self.0.borrow_mut().push(Ev::Save(power.state()));
            if self.1 {
                Err(SaveError::Write(StoreError::Io))
            } else {
                Ok(SaveReport {
                    slot: Slot::A,
                    seq: 1,
                })
            }
        }
    }
    struct FakeDisplay(Log, bool);
    impl DisplayPark for FakeDisplay {
        fn park(&mut self) -> Result<(), StepError> {
            self.0.borrow_mut().push(Ev::Park);
            if self.1 {
                Err(StepError("epd"))
            } else {
                Ok(())
            }
        }
    }
    struct FakeSd(Log, bool);
    impl SdShutdown for FakeSd {
        fn shutdown(&mut self) -> Result<(), StepError> {
            self.0.borrow_mut().push(Ev::SdStop);
            if self.1 { Err(StepError("sd")) } else { Ok(()) }
        }
    }
    struct FakeCharge(Log, bool);
    impl ChargeRestore for FakeCharge {
        fn restore(&mut self) -> Result<(), StepError> {
            self.0.borrow_mut().push(Ev::Charge);
            if self.1 {
                Err(StepError("chg"))
            } else {
                Ok(())
            }
        }
    }
    struct FakeLines(Log, bool);
    impl LineSilencer for FakeLines {
        fn silence(&mut self) -> Result<(), StepError> {
            self.0.borrow_mut().push(Ev::Silence);
            if self.1 {
                Err(StepError("lines"))
            } else {
                Ok(())
            }
        }
    }
    struct FakeWake(Log, bool);
    impl WakeConfig for FakeWake {
        fn arm(&mut self, spec: &WakeSpec) -> Result<(), WakeError> {
            self.0.borrow_mut().push(Ev::Wake(*spec));
            if self.1 {
                Err(WakeError::NoLowPowerPath)
            } else {
                Ok(())
            }
        }
    }
    struct FakeEntry(Log);
    impl SleepEntry for FakeEntry {
        type Entered = ();
        fn enter(self, _off: PoweredOffProof<'_>, wake: WakeArmed, _r: &SleepReport) {
            self.0.borrow_mut().push(Ev::Enter(wake.spec()));
        }
    }

    type Power = PeripheralPower<Pin>;

    fn booted(sd_active: bool) -> (Power, Log) {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let mut p = PeripheralPower::new(Pin(log.clone()));
        p.power_cycle(&mut NoDelay).unwrap();
        if sd_active {
            let permit: SdInitPermit = p.begin_sd_init().unwrap();
            p.finish_sd_init(permit, true);
        }
        log.borrow_mut().clear();
        (p, log)
    }

    type FakeSeq<'a> = SleepSequence<
        'a,
        Pin,
        FakeSaver,
        FakeDisplay,
        FakeSd,
        FakeCharge,
        FakeLines,
        FakeWake,
        FakeEntry,
    >;

    fn seq<'a>(power: &'a mut Power, log: &Log, f: Fail) -> FakeSeq<'a> {
        SleepSequence {
            power,
            saver: FakeSaver(log.clone(), f.save),
            display: FakeDisplay(log.clone(), f.park),
            sd: FakeSd(log.clone(), f.sd),
            charge: FakeCharge(log.clone(), f.charge),
            lines: FakeLines(log.clone(), f.lines),
            wake: FakeWake(log.clone(), f.wake),
            entry: FakeEntry(log.clone()),
        }
    }

    fn pos(log: &Log, e: &Ev) -> usize {
        log.borrow()
            .iter()
            .position(|x| x == e)
            .unwrap_or_else(|| panic!("event {e:?} missing in {:?}", log.borrow()))
    }

    fn trace(log: &Log) -> Vec<Ev> {
        log.borrow().clone()
    }

    // -- R19: order ----------------------------------------------------------

    #[test]
    fn r19_full_sequence_trace_in_bsp_order() {
        let (mut p, log) = booted(true);
        let (report, ()) = seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap();
        assert_eq!(
            trace(&log),
            vec![
                // R18: the position is written while SD is active, first
                Ev::Save(RailState::SdActive),
                // wake source armed before anything irreversible (module
                // comment: moved ahead of BSP :348-352 on purpose)
                Ev::Wake(WakeSpec {
                    gpio: 2,
                    level: WakeLevel::Low,
                    pull: WakePull::Up
                }),
                Ev::Park,             // BSP :337 board_display_sleep
                Ev::SdStop,           // flush before the card loses power
                Ev::Charge,           // GPIO10 = charging allowed
                Ev::Silence,          // BSP :340-344 SCLK/MOSI/CS/SD_CS/PDM_CLK low
                Ev::Gpio27Low,        // BSP :346 board_peripherals_power(false)
                Ev::Enter(WAKE_SPEC), // BSP :356 esp_deep_sleep_start
            ]
        );
        assert!(report.all_steps_ok());
        assert!(report.saved());
        assert_eq!(p.state(), RailState::PoweredOff);
    }

    #[test]
    fn r19_gpio27_is_never_driven_high_and_goes_low_exactly_once() {
        let (mut p, log) = booted(true);
        seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap();
        let t = trace(&log);
        assert_eq!(t.iter().filter(|e| **e == Ev::Gpio27Low).count(), 1);
        assert!(!t.contains(&Ev::Gpio27High));
    }

    #[test]
    fn r19_every_peripheral_step_precedes_the_gpio27_cut() {
        let (mut p, log) = booted(true);
        seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap();
        let cut = pos(&log, &Ev::Gpio27Low);
        for e in [
            Ev::Park,
            Ev::SdStop,
            Ev::Charge,
            Ev::Silence,
            Ev::Wake(WAKE_SPEC),
        ] {
            assert!(pos(&log, &e) < cut, "{e:?} must precede the GPIO27 cut");
        }
        assert!(pos(&log, &Ev::Enter(WAKE_SPEC)) > cut);
    }

    #[test]
    fn r19_park_precedes_silence_precedes_cut_like_the_bsp() {
        let (mut p, log) = booted(true);
        seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap();
        assert!(pos(&log, &Ev::Park) < pos(&log, &Ev::Silence));
        assert!(pos(&log, &Ev::Silence) < pos(&log, &Ev::Gpio27Low));
    }

    #[test]
    fn r19_sd_is_stopped_before_its_power_goes() {
        let (mut p, log) = booted(true);
        seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap();
        assert!(pos(&log, &Ev::SdStop) < pos(&log, &Ev::Gpio27Low));
    }

    // -- R18: save before power-down ----------------------------------------

    #[test]
    fn r18_save_is_the_first_event_and_runs_with_sd_active_and_rail_high() {
        let (mut p, log) = booted(true);
        seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap();
        assert_eq!(trace(&log)[0], Ev::Save(RailState::SdActive));
        // nothing that touches a peripheral or the rail happened before it
        assert_eq!(pos(&log, &Ev::Save(RailState::SdActive)), 0);
    }

    #[test]
    fn r18_save_precedes_every_power_down_event() {
        let (mut p, log) = booted(true);
        seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap();
        let save = pos(&log, &Ev::Save(RailState::SdActive));
        for e in [Ev::Park, Ev::SdStop, Ev::Silence, Ev::Gpio27Low] {
            assert!(save < pos(&log, &e));
        }
    }

    #[test]
    fn r18_save_is_refused_when_the_card_was_removed_and_sleep_still_happens() {
        // card pulled -> `card_removed` -> rail state PowerCycled (SD not active)
        let (mut p, log) = booted(true);
        p.card_removed().unwrap();
        let mut store = MemStore::default();
        let state = SessionState::home();
        let report = {
            let s = SleepSequence {
                power: &mut p,
                saver: StoreSaver {
                    store: &mut store,
                    state: &state,
                },
                display: FakeDisplay(log.clone(), false),
                sd: FakeSd(log.clone(), false),
                charge: FakeCharge(log.clone(), false),
                lines: FakeLines(log.clone(), false),
                wake: FakeWake(log.clone(), false),
                entry: FakeEntry(log.clone()),
            };
            s.enter_deep_sleep().unwrap().0
        };
        assert_eq!(
            report.save,
            Err(SaveError::SdNotActive(RailState::PowerCycled))
        );
        assert!(!report.saved());
        assert_eq!(store.io, 0, "a refused save must not touch the card");
        assert_eq!(p.state(), RailState::PoweredOff, "still slept");
        assert!(trace(&log).contains(&Ev::Enter(WAKE_SPEC)));
        assert!(pos(&log, &Ev::Wake(WAKE_SPEC)) < pos(&log, &Ev::Gpio27Low));
    }

    #[test]
    fn r18_save_failure_is_reported_and_the_device_still_sleeps() {
        // policy: sleep anyway (module comment)
        let (mut p, log) = booted(true);
        let f = Fail {
            save: true,
            ..Fail::default()
        };
        let (report, ()) = seq(&mut p, &log, f).enter_deep_sleep().unwrap();
        assert_eq!(report.save, Err(SaveError::Write(StoreError::Io)));
        assert!(!report.saved());
        assert!(report.display.is_ok() && report.sd.is_ok());
        assert_eq!(p.state(), RailState::PoweredOff);
        assert!(trace(&log).contains(&Ev::Enter(WAKE_SPEC)));
    }

    #[test]
    fn r18_a_real_store_failure_surfaces_through_the_sequence() {
        let (mut p, log) = booted(true);
        let mut store = MemStore {
            fail_write: true,
            ..MemStore::default()
        };
        let state = SessionState::home();
        let s = SleepSequence {
            power: &mut p,
            saver: StoreSaver {
                store: &mut store,
                state: &state,
            },
            display: FakeDisplay(log.clone(), false),
            sd: FakeSd(log.clone(), false),
            charge: FakeCharge(log.clone(), false),
            lines: FakeLines(log.clone(), false),
            wake: FakeWake(log.clone(), false),
            entry: FakeEntry(log.clone()),
        };
        let (report, ()) = s.enter_deep_sleep().unwrap();
        assert_eq!(report.save, Err(SaveError::Write(StoreError::Io)));
        assert_eq!(p.state(), RailState::PoweredOff);
    }

    // -- R19: failure matrix -------------------------------------------------

    fn assert_reached_safe_sleep(log: &Log, p: &Power, report: &SleepReport) {
        let t = trace(log);
        // every step was attempted exactly once, in order
        for e in [
            Ev::Save(RailState::SdActive),
            Ev::Wake(WAKE_SPEC),
            Ev::Park,
            Ev::SdStop,
            Ev::Charge,
            Ev::Silence,
            Ev::Gpio27Low,
            Ev::Enter(WAKE_SPEC),
        ] {
            assert_eq!(t.iter().filter(|x| **x == e).count(), 1, "{e:?} in {t:?}");
        }
        // wake armed, then rail cut, then sleep
        assert!(pos(log, &Ev::Wake(WAKE_SPEC)) < pos(log, &Ev::Gpio27Low));
        assert!(pos(log, &Ev::Gpio27Low) < pos(log, &Ev::Enter(WAKE_SPEC)));
        assert_eq!(p.state(), RailState::PoweredOff);
        let _ = report;
    }

    #[test]
    fn r19_each_single_step_failure_still_reaches_wake_armed_rail_off_sleep() {
        let cases: [(&str, Fail); 5] = [
            (
                "save",
                Fail {
                    save: true,
                    ..Fail::default()
                },
            ),
            (
                "park",
                Fail {
                    park: true,
                    ..Fail::default()
                },
            ),
            (
                "sd",
                Fail {
                    sd: true,
                    ..Fail::default()
                },
            ),
            (
                "charge",
                Fail {
                    charge: true,
                    ..Fail::default()
                },
            ),
            (
                "lines",
                Fail {
                    lines: true,
                    ..Fail::default()
                },
            ),
        ];
        for (name, f) in cases {
            let (mut p, log) = booted(true);
            let (report, ()) = seq(&mut p, &log, f)
                .enter_deep_sleep()
                .unwrap_or_else(|a| panic!("{name} failure aborted the sequence: {a:?}"));
            assert_reached_safe_sleep(&log, &p, &report);
            assert!(!report.all_steps_ok(), "{name}");
        }
    }

    #[test]
    fn r19_failure_is_recorded_in_the_matching_report_field_only() {
        let (mut p, log) = booted(true);
        let f = Fail {
            park: true,
            ..Fail::default()
        };
        let (r, ()) = seq(&mut p, &log, f).enter_deep_sleep().unwrap();
        assert_eq!(r.display, Err(StepError("epd")));
        assert!(r.save.is_ok() && r.sd.is_ok() && r.charge.is_ok() && r.lines.is_ok());
    }

    #[test]
    fn r19_all_peripheral_steps_failing_together_still_sleeps() {
        let (mut p, log) = booted(true);
        let f = Fail {
            save: true,
            park: true,
            sd: true,
            charge: true,
            lines: true,
            wake: false,
        };
        let (report, ()) = seq(&mut p, &log, f).enter_deep_sleep().unwrap();
        assert_reached_safe_sleep(&log, &p, &report);
        assert!(report.save.is_err() && report.display.is_err());
        assert!(report.sd.is_err() && report.charge.is_err() && report.lines.is_err());
    }

    #[test]
    fn r19_wake_config_failure_aborts_before_any_irreversible_step() {
        let (mut p, log) = booted(true);
        let f = Fail {
            wake: true,
            ..Fail::default()
        };
        let abort = seq(&mut p, &log, f).enter_deep_sleep().unwrap_err();
        assert_eq!(abort.reason, AbortReason::Wake(WakeError::NoLowPowerPath));
        // the position was saved, nothing else happened
        assert!(matches!(abort.save, Some(Ok(_))));
        assert_eq!(
            trace(&log),
            vec![Ev::Save(RailState::SdActive), Ev::Wake(WAKE_SPEC)]
        );
        // rail untouched and still usable: SD active, no GPIO27 event
        assert_eq!(p.state(), RailState::SdActive);
        assert!(p.sd_active().is_ok());
    }

    struct HeldKeyWake(Log);
    impl WakeConfig for HeldKeyWake {
        fn arm(&mut self, spec: &WakeSpec) -> Result<(), WakeError> {
            self.0.borrow_mut().push(Ev::Wake(*spec));
            Err(WakeError::AlreadyAsserted)
        }
    }

    #[test]
    fn r19_a_held_wake_key_aborts_cleanly_and_a_retry_after_release_sleeps() {
        let (mut p, log) = booted(true);
        let held = SleepSequence {
            power: &mut p,
            saver: FakeSaver(log.clone(), false),
            display: FakeDisplay(log.clone(), false),
            sd: FakeSd(log.clone(), false),
            charge: FakeCharge(log.clone(), false),
            lines: FakeLines(log.clone(), false),
            wake: HeldKeyWake(log.clone()),
            entry: FakeEntry(log.clone()),
        };
        let abort = held.enter_deep_sleep().unwrap_err();
        assert_eq!(abort.reason, AbortReason::Wake(WakeError::AlreadyAsserted));
        assert_eq!(p.state(), RailState::SdActive, "nothing was shut down");
        assert!(!trace(&log).contains(&Ev::Park));
        assert!(!trace(&log).contains(&Ev::Gpio27Low));
        // key released: the same rail can sleep
        log.borrow_mut().clear();
        seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap();
        assert_eq!(p.state(), RailState::PoweredOff);
    }

    // -- R19: wake spec ------------------------------------------------------

    #[test]
    fn r19_wake_spec_is_gpio2_active_low_with_pull_up_as_in_the_bsp() {
        // board_c61.c:66 PIN_KEY_WAKE = GPIO2 (active-low), :352 GPIO_WAKEUP_GPIO_LOW,
        // board_keys.c:97-99 active_level 0 + internal pull-up
        assert_eq!(WAKE_SPEC.gpio, 2);
        assert_eq!(WAKE_SPEC.level, WakeLevel::Low);
        assert_eq!(WAKE_SPEC.pull, WakePull::Up);
        // a low-level wake needs the pull AGAINST the wake level
        assert_ne!(
            (WAKE_SPEC.level, WAKE_SPEC.pull),
            (WakeLevel::Low, WakePull::Down)
        );
    }

    #[test]
    fn r19_the_sequence_arms_exactly_the_bsp_spec_once_and_enters_with_it() {
        let (mut p, log) = booted(true);
        seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap();
        let arms: Vec<_> = trace(&log)
            .into_iter()
            .filter(|e| matches!(e, Ev::Wake(_)))
            .collect();
        assert_eq!(arms, vec![Ev::Wake(WAKE_SPEC)]);
        assert!(trace(&log).contains(&Ev::Enter(WAKE_SPEC)));
    }

    // -- R19: state guards ---------------------------------------------------

    #[test]
    fn r19_a_rail_that_cannot_shut_down_aborts_without_any_side_effect() {
        // Unpowered
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let mut p = PeripheralPower::new(Pin(log.clone()));
        let a = seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap_err();
        assert_eq!(
            a,
            SleepAbort {
                reason: AbortReason::Rail(PowerError::InvalidState(RailState::Unpowered)),
                save: None
            }
        );
        assert!(log.borrow().is_empty());

        // SdInitializing
        let (mut p, log) = booted(false);
        let _permit = p.begin_sd_init().unwrap();
        let a = seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap_err();
        assert_eq!(
            a.reason,
            AbortReason::Rail(PowerError::InvalidState(RailState::SdInitializing))
        );
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn r19_a_second_sequence_after_the_rail_is_off_is_refused_and_does_nothing() {
        let (mut p, log) = booted(true);
        seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap();
        log.borrow_mut().clear();
        for _ in 0..2 {
            let a = seq(&mut p, &log, Fail::default())
                .enter_deep_sleep()
                .unwrap_err();
            assert_eq!(
                a.reason,
                AbortReason::Rail(PowerError::InvalidState(RailState::PoweredOff))
            );
        }
        assert!(log.borrow().is_empty(), "no re-arm, no second cut");
    }

    #[test]
    fn r19_sequence_works_with_the_sd_not_yet_initialised() {
        // no card at boot: PowerCycled. There is nothing to save, but the board
        // must still be able to sleep.
        let (mut p, log) = booted(false);
        let (report, ()) = seq(&mut p, &log, Fail::default())
            .enter_deep_sleep()
            .unwrap();
        assert_eq!(trace(&log)[0], Ev::Save(RailState::PowerCycled));
        assert_eq!(p.state(), RailState::PoweredOff);
        assert!(report.all_steps_ok()); // the fake saver does not look at the state
    }

    // -- charge restore adapter ----------------------------------------------

    struct Chg(Rc<RefCell<Vec<bool>>>);
    impl ChargePin for Chg {
        fn set_charging(&mut self, e: bool) {
            self.0.borrow_mut().push(e);
        }
    }
    struct NoAdc;
    impl AdcSample for NoAdc {
        fn sample_mv(&mut self) -> Option<u16> {
            None
        }
    }

    #[test]
    fn r19_charge_restore_leaves_gpio10_at_charging_allowed_even_after_a_failed_measure() {
        let writes = Rc::new(RefCell::new(Vec::new()));
        let mut m = BatteryMonitor::new(Chg(writes.clone()), NoAdc, NoDelay);
        assert!(m.measure().is_err()); // pause ... resume
        writes.borrow_mut().clear();
        m.restore().unwrap();
        assert_eq!(*writes.borrow(), vec![true]);
    }

    #[test]
    fn r19_missing_battery_monitor_is_a_recorded_failure_not_a_stop() {
        let mut none: Option<BatteryMonitor<Chg, NoAdc, NoDelay>> = None;
        assert!(none.restore().is_err());
    }

    // -- R20: wake cause and boot plan ---------------------------------------

    fn bits(key: bool, timer: bool, other: bool) -> WakeBits {
        WakeBits {
            key_path: key,
            timer,
            other,
        }
    }

    #[test]
    fn r20_wake_cause_classification_matches_the_bsp_priority() {
        assert_eq!(classify_wake(bits(false, false, false)), WakeCause::PowerOn);
        assert_eq!(classify_wake(bits(true, false, false)), WakeCause::Key);
        assert_eq!(classify_wake(bits(false, true, false)), WakeCause::Timer);
        assert_eq!(classify_wake(bits(false, false, true)), WakeCause::Other);
        // BSP: GPIO beats timer beats other
        assert_eq!(classify_wake(bits(true, true, true)), WakeCause::Key);
        assert_eq!(classify_wake(bits(false, true, true)), WakeCause::Timer);
        assert_eq!(classify_wake(WakeBits::default()), WakeCause::PowerOn);
    }

    fn restored() -> BootDecision {
        let mut state = SessionState::home();
        state.wake_count = 7;
        BootDecision::Restore(Restored {
            slot: Slot::B,
            seq: 9,
            state,
        })
    }

    #[test]
    fn r20_boot_plan_decision_table_all_causes_by_all_session_outcomes() {
        let causes = [
            WakeCause::PowerOn,
            WakeCause::Key,
            WakeCause::Timer,
            WakeCause::Other,
        ];
        let normals = [
            NormalBootReason::NoSession,
            NormalBootReason::Corrupt(DecodeError::BadCrc),
            NormalBootReason::Corrupt(DecodeError::BadMagic),
            NormalBootReason::StorageUnavailable(StoreError::Io),
            NormalBootReason::SdNotActive(RailState::PowerCycled),
        ];
        for cause in causes {
            // valid session -> restore, whatever the cause
            let plan = plan_boot(cause, restored());
            assert!(plan.is_restore(), "{cause:?}");
            assert_eq!(plan.cause(), cause);
            // anything else -> normal boot, with the reason kept
            for reason in normals {
                let plan = plan_boot(cause, BootDecision::NormalBoot(reason));
                assert_eq!(plan, BootPlan::Normal { cause, reason });
                assert!(!plan.is_restore());
            }
        }
    }

    #[test]
    fn r20_restore_plan_carries_the_exact_saved_position() {
        let BootPlan::Restore { restored: r, .. } = plan_boot(WakeCause::Key, restored()) else {
            panic!("expected restore")
        };
        assert_eq!((r.slot, r.seq, r.state.wake_count), (Slot::B, 9, 7));
    }

    // -- R18 + R20 end to end: sleep, "reboot", restore ------------------------

    #[derive(Default)]
    struct MemStore {
        a: Option<Vec<u8>>,
        b: Option<Vec<u8>>,
        io: u32,
        fail_write: bool,
    }
    impl SessionStore for MemStore {
        fn read(&mut self, slot: Slot, buf: &mut [u8]) -> Result<usize, StoreError> {
            self.io += 1;
            let f = match slot {
                Slot::A => &self.a,
                Slot::B => &self.b,
            };
            match f {
                None => Err(StoreError::NotFound),
                Some(d) => {
                    let n = d.len().min(buf.len());
                    buf[..n].copy_from_slice(&d[..n]);
                    Ok(n)
                }
            }
        }
        fn write(&mut self, slot: Slot, data: &[u8]) -> Result<(), StoreError> {
            self.io += 1;
            if self.fail_write {
                return Err(StoreError::Io);
            }
            match slot {
                Slot::A => self.a = Some(data.to_vec()),
                Slot::B => self.b = Some(data.to_vec()),
            }
            Ok(())
        }
        fn delete(&mut self, slot: Slot) -> Result<(), StoreError> {
            self.io += 1;
            match slot {
                Slot::A => self.a = None,
                Slot::B => self.b = None,
            }
            Ok(())
        }
    }

    fn reader_state() -> SessionState {
        let mut s = SessionState::home();
        s.nav_depth = 2;
        s.nav_stack = [0, 2, 0, 0];
        s.set_reader_filename(b"BOOK.EPU").unwrap();
        s.reader_is_epub = true;
        s.reader_chapter = 12;
        s.reader_page = 3;
        s.reader_byte_offset = 123_456;
        s.reader_font_size = 2;
        s.wake_count = 4;
        s
    }

    #[test]
    fn r18_r20_save_sleep_reboot_restores_the_same_reading_position() {
        let mut store = MemStore::default();
        let state = reader_state();

        // boot 1: running, then sleep
        let (mut p, log) = booted(true);
        let s = SleepSequence {
            power: &mut p,
            saver: StoreSaver {
                store: &mut store,
                state: &state,
            },
            display: FakeDisplay(log.clone(), false),
            sd: FakeSd(log.clone(), false),
            charge: FakeCharge(log.clone(), false),
            lines: FakeLines(log.clone(), false),
            wake: FakeWake(log.clone(), false),
            entry: FakeEntry(log.clone()),
        };
        let (report, ()) = s.enter_deep_sleep().unwrap();
        assert!(report.saved());
        // after the cut nothing can be restored or saved any more (R7/R18)
        assert_eq!(
            restore_session(&p, &mut store),
            BootDecision::NormalBoot(NormalBootReason::SdNotActive(RailState::PoweredOff))
        );

        // boot 2 (key wake): fresh state machine, power-cycle, SD init, restore
        let (p2, _log2) = booted(true);
        let decision = restore_session(&p2, &mut store);
        let plan = plan_boot(WakeCause::Key, decision);
        let BootPlan::Restore { restored, cause } = plan else {
            panic!("expected restore, got {plan:?}")
        };
        assert_eq!(cause, WakeCause::Key);
        assert_eq!(restored.state, state);
    }

    #[test]
    fn r20_a_failed_save_before_sleep_boots_normally_next_time() {
        let mut store = MemStore {
            fail_write: true,
            ..MemStore::default()
        };
        let state = reader_state();
        let (mut p, log) = booted(true);
        let s = SleepSequence {
            power: &mut p,
            saver: StoreSaver {
                store: &mut store,
                state: &state,
            },
            display: FakeDisplay(log.clone(), false),
            sd: FakeSd(log.clone(), false),
            charge: FakeCharge(log.clone(), false),
            lines: FakeLines(log.clone(), false),
            wake: FakeWake(log.clone(), false),
            entry: FakeEntry(log.clone()),
        };
        let (report, ()) = s.enter_deep_sleep().unwrap();
        assert!(!report.saved());
        store.fail_write = false;
        let (p2, _l) = booted(true);
        let plan = plan_boot(WakeCause::Key, restore_session(&p2, &mut store));
        assert!(!plan.is_restore());
    }
}
