// GPIO27 peripheral-power / EPD-reset contract (R6, R7).
//
// On the OnePage C61, GPIO27 is the EPD RST line AND the SD/MIC power enable
// (high = powered; ../bsp_onepage_c61/board_c61.c:41, 332). Consequences:
//   * boot: one power-cycle (low -> delay -> high -> delay) BEFORE any SD
//     access (board_c61.c:80-84, "low 20ms -> high 20ms");
//   * runtime: once the card is powered, GPIO27 must stay high. A hardware
//     EPD reset would pull SD power; display reset is software-only;
//   * shutdown: GPIO27 goes low only at the very end, after the EPD is parked
//     and the shared SPI/PDM lines are silenced (board_c61.c:337-346).
//
// This state machine is the only code allowed to drive GPIO27. The kernel owns
// the pin inside it and never hands it out, so there is no other path.

/// GPIO27 settle time with the rail low, ms (board_c61.c:81-82).
pub const POWER_CYCLE_LOW_MS: u32 = 20;
/// GPIO27 settle time after the rail goes high, ms (board_c61.c:83-84).
pub const POWER_CYCLE_HIGH_MS: u32 = 20;

/// Output driver for GPIO27 (implemented over esp-hal `Output` in the kernel).
pub trait RailPin {
    fn set_high(&mut self);
    fn set_low(&mut self);
}

/// Blocking delay (esp-hal `Delay` in the kernel).
pub trait DelayMs {
    fn delay_ms(&mut self, ms: u32);
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RailState {
    /// Boot: pin not yet driven by this state machine; SD/EPD unusable.
    Unpowered,
    /// Power-cycle done, rail high. SD init may start.
    PowerCycled,
    /// An `SdInitPermit` is outstanding.
    SdInitializing,
    /// SD initialised; rail must stay high.
    SdActive,
    /// Shutdown started (no new SD init); rail still high so the caller can
    /// park EPD / SD and silence the shared lines.
    ShuttingDown,
    /// Rail low. Terminal (the chip resets out of deep sleep).
    PoweredOff,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PowerError {
    /// Operation not allowed in the current state; nothing was touched.
    InvalidState(RailState),
    /// A hardware EPD reset would toggle GPIO27 and drop SD power.
    HardwareResetForbidden,
}

/// How the caller must reset the display. Hardware reset is deliberately not
/// a variant: see `PowerError::HardwareResetForbidden`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DisplayReset {
    /// Issue the SSD1677 software reset command (0x12); do not touch GPIO27.
    Software,
}

/// Proof that the power-cycle completed. Only `begin_sd_init` mints one (SD
/// init code, T5, must require it); it is not `Clone`/`Copy` and is consumed
/// by `finish_sd_init`.
#[must_use = "pass the permit to finish_sd_init after the SD init attempt"]
#[derive(Debug)]
pub struct SdInitPermit(());

/// Proof that GPIO27 is low and the rail is terminal (`PoweredOff`). Borrows
/// the state machine, so it cannot outlive (or coexist with a `&mut` use of) the
/// `PeripheralPower`. Deep-sleep entry (`sleep::SleepEntry`) demands one, which
/// makes "sleep while SD is still powered" unrepresentable. Not `Clone`/`Copy`;
/// only `PeripheralPower::powered_off` creates it.
#[must_use = "pass the proof to the deep-sleep entry"]
#[derive(Debug)]
pub struct PoweredOffProof<'a>(core::marker::PhantomData<&'a ()>);

pub struct PeripheralPower<P: RailPin> {
    pin: P,
    state: RailState,
}

impl<P: RailPin> PeripheralPower<P> {
    /// Takes ownership of GPIO27. Does not drive it.
    pub fn new(pin: P) -> Self {
        Self {
            pin,
            state: RailState::Unpowered,
        }
    }

    pub fn state(&self) -> RailState {
        self.state
    }

    /// Boot power-cycle: low, delay, high, delay. Allowed exactly once, from
    /// `Unpowered`. Anywhere else it is rejected WITHOUT touching the pin, so
    /// a stray call can never drop SD power (R7).
    pub fn power_cycle<D: DelayMs>(&mut self, delay: &mut D) -> Result<(), PowerError> {
        if self.state != RailState::Unpowered {
            return Err(PowerError::InvalidState(self.state));
        }
        self.pin.set_low();
        delay.delay_ms(POWER_CYCLE_LOW_MS);
        self.pin.set_high();
        delay.delay_ms(POWER_CYCLE_HIGH_MS);
        self.state = RailState::PowerCycled;
        Ok(())
    }

    /// Start SD init. Requires a completed power-cycle; one attempt at a time.
    pub fn begin_sd_init(&mut self) -> Result<SdInitPermit, PowerError> {
        if self.state != RailState::PowerCycled {
            return Err(PowerError::InvalidState(self.state));
        }
        self.state = RailState::SdInitializing;
        Ok(SdInitPermit(()))
    }

    /// Report the SD init result. Success -> `SdActive`; failure returns to
    /// `PowerCycled` (rail stays high so a later retry/card insert works).
    pub fn finish_sd_init(&mut self, permit: SdInitPermit, success: bool) {
        let SdInitPermit(()) = permit;
        self.state = if success {
            RailState::SdActive
        } else {
            RailState::PowerCycled
        };
    }

    /// The card was removed (card-detect, T5). Returns `SdActive` to
    /// `PowerCycled` so a re-inserted card can be initialised again through a
    /// fresh `SdInitPermit`. The rail is NOT touched (R7: GPIO27 stays high
    /// while the system runs). No-op in `PowerCycled`; rejected elsewhere.
    pub fn card_removed(&mut self) -> Result<(), PowerError> {
        match self.state {
            RailState::SdActive => {
                self.state = RailState::PowerCycled;
                Ok(())
            }
            RailState::PowerCycled => Ok(()),
            s => Err(PowerError::InvalidState(s)),
        }
    }

    /// Runtime display reset policy. Always software; never touches GPIO27.
    /// Rejected while the rail is not up (before power-cycle / after shutdown).
    pub fn display_reset(&mut self) -> Result<DisplayReset, PowerError> {
        match self.state {
            RailState::PowerCycled | RailState::SdInitializing | RailState::SdActive => {
                Ok(DisplayReset::Software)
            }
            s => Err(PowerError::InvalidState(s)),
        }
    }

    /// Explicit tripwire for a GPIO27-based EPD reset: always refused, pin
    /// untouched. (The one hardware reset is the boot power-cycle.)
    pub fn hardware_reset_display(&mut self) -> Result<(), PowerError> {
        Err(PowerError::HardwareResetForbidden)
    }

    /// Enter the shutdown phase. Rail stays high. Not allowed before
    /// power-cycle, while an SD init is in flight, or twice.
    pub fn begin_shutdown(&mut self) -> Result<(), PowerError> {
        match self.state {
            RailState::PowerCycled | RailState::SdActive => {
                self.state = RailState::ShuttingDown;
                Ok(())
            }
            s => Err(PowerError::InvalidState(s)),
        }
    }

    /// `Ok` only in `PoweredOff`, i.e. after `cut_peripheral_power`.
    pub fn powered_off(&self) -> Result<PoweredOffProof<'_>, PowerError> {
        if self.state != RailState::PoweredOff {
            return Err(PowerError::InvalidState(self.state));
        }
        Ok(PoweredOffProof(core::marker::PhantomData))
    }

    /// Drive GPIO27 low. Only valid in the shutdown phase, i.e. after the
    /// caller has parked EPD/SD and silenced SCK/MOSI/CS/PDM (board_c61.c:337-346).
    pub fn cut_peripheral_power(&mut self) -> Result<(), PowerError> {
        if self.state != RailState::ShuttingDown {
            return Err(PowerError::InvalidState(self.state));
        }
        self.pin.set_low();
        self.state = RailState::PoweredOff;
        Ok(())
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
        Low,
        High,
        Delay(u32),
        SdInit, // marker pushed by the fake "SD driver"
    }

    type Log = Rc<RefCell<Vec<Ev>>>;

    struct FakePin(Log);
    impl RailPin for FakePin {
        fn set_high(&mut self) {
            self.0.borrow_mut().push(Ev::High);
        }
        fn set_low(&mut self) {
            self.0.borrow_mut().push(Ev::Low);
        }
    }
    struct FakeDelay(Log);
    impl DelayMs for FakeDelay {
        fn delay_ms(&mut self, ms: u32) {
            self.0.borrow_mut().push(Ev::Delay(ms));
        }
    }

    fn rig() -> (PeripheralPower<FakePin>, FakeDelay, Log) {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        (
            PeripheralPower::new(FakePin(log.clone())),
            FakeDelay(log.clone()),
            log,
        )
    }

    // stand-in for the T5 SD init: it can only run with a permit
    fn fake_sd_init(_permit: &SdInitPermit, log: &Log) {
        log.borrow_mut().push(Ev::SdInit);
    }

    fn up() -> (PeripheralPower<FakePin>, FakeDelay, Log) {
        let (mut p, mut d, log) = rig();
        p.power_cycle(&mut d).unwrap();
        (p, d, log)
    }

    fn pin_events(log: &Log) -> usize {
        log.borrow()
            .iter()
            .filter(|e| matches!(e, Ev::Low | Ev::High))
            .count()
    }

    #[test]
    fn r6_new_does_not_touch_gpio27() {
        let (p, _d, log) = rig();
        assert_eq!(p.state(), RailState::Unpowered);
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn r6_power_cycle_is_low_delay_high_delay_with_bsp_timing() {
        let (p, _d, log) = up();
        assert_eq!(
            *log.borrow(),
            vec![Ev::Low, Ev::Delay(20), Ev::High, Ev::Delay(20)]
        );
        assert_eq!((POWER_CYCLE_LOW_MS, POWER_CYCLE_HIGH_MS), (20, 20));
        assert_eq!(p.state(), RailState::PowerCycled);
    }

    #[test]
    fn r6_sd_init_happens_after_all_power_cycle_events() {
        let (mut p, mut d, log) = rig();
        p.power_cycle(&mut d).unwrap();
        let permit = p.begin_sd_init().unwrap();
        fake_sd_init(&permit, &log);
        p.finish_sd_init(permit, true);
        let l = log.borrow();
        let sd = l.iter().position(|e| *e == Ev::SdInit).unwrap();
        assert_eq!(&l[..sd], &[Ev::Low, Ev::Delay(20), Ev::High, Ev::Delay(20)]);
        assert_eq!(l.len(), sd + 1, "no GPIO27 event after SD init");
    }

    #[test]
    fn r6_sd_init_before_power_cycle_is_rejected() {
        let (mut p, _d, log) = rig();
        assert_eq!(
            p.begin_sd_init().unwrap_err(),
            PowerError::InvalidState(RailState::Unpowered)
        );
        assert_eq!(p.state(), RailState::Unpowered);
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn r6_second_power_cycle_is_rejected_without_gpio_activity() {
        let (mut p, mut d, log) = up();
        let before = log.borrow().len();
        assert_eq!(
            p.power_cycle(&mut d).unwrap_err(),
            PowerError::InvalidState(RailState::PowerCycled)
        );
        assert_eq!(log.borrow().len(), before);
    }

    #[test]
    fn r6_duplicate_sd_init_is_rejected_while_in_flight_and_after_success() {
        let (mut p, _d, _log) = up();
        let permit = p.begin_sd_init().unwrap();
        assert_eq!(
            p.begin_sd_init().unwrap_err(),
            PowerError::InvalidState(RailState::SdInitializing)
        );
        p.finish_sd_init(permit, true);
        assert_eq!(p.state(), RailState::SdActive);
        assert_eq!(
            p.begin_sd_init().unwrap_err(),
            PowerError::InvalidState(RailState::SdActive)
        );
    }

    #[test]
    fn r6_failed_sd_init_keeps_rail_high_and_allows_retry() {
        let (mut p, _d, log) = up();
        let before = log.borrow().len();
        let permit = p.begin_sd_init().unwrap();
        p.finish_sd_init(permit, false);
        assert_eq!(p.state(), RailState::PowerCycled);
        let permit = p.begin_sd_init().unwrap();
        p.finish_sd_init(permit, true);
        assert_eq!(p.state(), RailState::SdActive);
        assert_eq!(log.borrow().len(), before, "retry must not toggle GPIO27");
    }

    #[test]
    fn r7_runtime_reset_after_sd_init_never_touches_gpio27() {
        let (mut p, _d, log) = up();
        let permit = p.begin_sd_init().unwrap();
        p.finish_sd_init(permit, true);
        let before = log.borrow().len();
        for _ in 0..3 {
            assert_eq!(p.display_reset(), Ok(DisplayReset::Software));
        }
        assert_eq!(log.borrow().len(), before);
        assert_eq!(p.state(), RailState::SdActive);
    }

    #[test]
    fn r7_hardware_reset_path_is_refused_in_every_state() {
        let (mut p, mut d, log) = rig();
        assert_eq!(
            p.hardware_reset_display(),
            Err(PowerError::HardwareResetForbidden)
        );
        p.power_cycle(&mut d).unwrap();
        assert_eq!(
            p.hardware_reset_display(),
            Err(PowerError::HardwareResetForbidden)
        );
        let permit = p.begin_sd_init().unwrap();
        p.finish_sd_init(permit, true);
        assert_eq!(
            p.hardware_reset_display(),
            Err(PowerError::HardwareResetForbidden)
        );
        p.begin_shutdown().unwrap();
        assert_eq!(
            p.hardware_reset_display(),
            Err(PowerError::HardwareResetForbidden)
        );
        // only the boot power-cycle (Low, High) and nothing from the refusals
        assert_eq!(pin_events(&log), 2);
    }

    #[test]
    fn r7_display_reset_refused_when_rail_not_up() {
        let (mut p, _d, log) = rig();
        assert_eq!(
            p.display_reset(),
            Err(PowerError::InvalidState(RailState::Unpowered))
        );
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn r7_rail_stays_high_from_sd_init_until_shutdown_cut() {
        let (mut p, _d, log) = up();
        let permit = p.begin_sd_init().unwrap();
        p.finish_sd_init(permit, true);
        // the last pin event so far is High, and nothing may follow until cut
        assert_eq!(
            log.borrow()
                .iter()
                .rev()
                .find(|e| matches!(e, Ev::Low | Ev::High)),
            Some(&Ev::High)
        );
        let n = log.borrow().len();
        p.begin_shutdown().unwrap();
        assert_eq!(log.borrow().len(), n, "begin_shutdown keeps the rail high");
    }

    #[test]
    fn r7_gpio27_goes_low_only_after_shutdown_begins() {
        let (mut p, _d, log) = up();
        let permit = p.begin_sd_init().unwrap();
        p.finish_sd_init(permit, true);
        let n = log.borrow().len();
        assert_eq!(
            p.cut_peripheral_power().unwrap_err(),
            PowerError::InvalidState(RailState::SdActive)
        );
        assert_eq!(log.borrow().len(), n);
        p.begin_shutdown().unwrap();
        p.cut_peripheral_power().unwrap();
        assert_eq!(p.state(), RailState::PoweredOff);
        assert_eq!(log.borrow().last(), Some(&Ev::Low));
        assert_eq!(log.borrow().len(), n + 1);
    }

    #[test]
    fn r7_cut_power_before_power_cycle_is_rejected() {
        let (mut p, _d, log) = rig();
        assert_eq!(
            p.cut_peripheral_power().unwrap_err(),
            PowerError::InvalidState(RailState::Unpowered)
        );
        assert_eq!(
            p.begin_shutdown().unwrap_err(),
            PowerError::InvalidState(RailState::Unpowered)
        );
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn r7_shutdown_blocks_sd_init_display_reset_and_second_power_cycle() {
        let (mut p, mut d, log) = up();
        p.begin_shutdown().unwrap();
        let n = log.borrow().len();
        let sd = RailState::ShuttingDown;
        assert_eq!(p.begin_sd_init().unwrap_err(), PowerError::InvalidState(sd));
        assert_eq!(p.display_reset(), Err(PowerError::InvalidState(sd)));
        assert_eq!(
            p.begin_shutdown().unwrap_err(),
            PowerError::InvalidState(sd)
        );
        assert_eq!(
            p.power_cycle(&mut d).unwrap_err(),
            PowerError::InvalidState(sd)
        );
        assert_eq!(log.borrow().len(), n);
    }

    #[test]
    fn r7_shutdown_rejected_while_sd_init_in_flight() {
        let (mut p, _d, _log) = up();
        let permit = p.begin_sd_init().unwrap();
        assert_eq!(
            p.begin_shutdown().unwrap_err(),
            PowerError::InvalidState(RailState::SdInitializing)
        );
        p.finish_sd_init(permit, false);
        assert!(p.begin_shutdown().is_ok());
    }

    #[test]
    fn r7_powered_off_is_terminal() {
        let (mut p, mut d, log) = up();
        p.begin_shutdown().unwrap();
        p.cut_peripheral_power().unwrap();
        let n = log.borrow().len();
        let off = RailState::PoweredOff;
        assert_eq!(
            p.power_cycle(&mut d).unwrap_err(),
            PowerError::InvalidState(off)
        );
        assert_eq!(
            p.cut_peripheral_power().unwrap_err(),
            PowerError::InvalidState(off)
        );
        assert_eq!(
            p.begin_sd_init().unwrap_err(),
            PowerError::InvalidState(off)
        );
        assert_eq!(p.display_reset(), Err(PowerError::InvalidState(off)));
        assert_eq!(log.borrow().len(), n);
    }

    #[test]
    fn r7_card_removed_reopens_sd_init_without_touching_gpio27() {
        let (mut p, _d, log) = up();
        let permit = p.begin_sd_init().unwrap();
        p.finish_sd_init(permit, true);
        let n = log.borrow().len();
        p.card_removed().unwrap();
        assert_eq!(p.state(), RailState::PowerCycled);
        let permit = p.begin_sd_init().unwrap();
        p.finish_sd_init(permit, true);
        assert_eq!(p.state(), RailState::SdActive);
        assert_eq!(log.borrow().len(), n, "re-insert must not toggle GPIO27");
    }

    #[test]
    fn r7_card_removed_is_rejected_when_rail_not_up_or_shutting_down() {
        let (mut p, _d, log) = rig();
        assert_eq!(
            p.card_removed().unwrap_err(),
            PowerError::InvalidState(RailState::Unpowered)
        );
        assert!(log.borrow().is_empty());
        let (mut p, _d, _log) = up();
        p.begin_shutdown().unwrap();
        assert_eq!(
            p.card_removed().unwrap_err(),
            PowerError::InvalidState(RailState::ShuttingDown)
        );
    }

    #[test]
    fn r19_powered_off_proof_exists_only_after_the_cut() {
        let (mut p, _d, _log) = up();
        assert_eq!(
            p.powered_off().unwrap_err(),
            PowerError::InvalidState(RailState::PowerCycled)
        );
        p.begin_shutdown().unwrap();
        assert_eq!(
            p.powered_off().unwrap_err(),
            PowerError::InvalidState(RailState::ShuttingDown)
        );
        p.cut_peripheral_power().unwrap();
        assert!(p.powered_off().is_ok());
    }
}
