// OnePage C61 deep sleep (R18, R19, R20): esp-hal adapters only.
//
// The order of everything (save -> wake -> shutdown -> park EPD -> stop SD ->
// charge -> silence lines -> GPIO27 cut -> sleep), the failure policy and the
// wake-cause / boot decision are in pulp_board_logic::sleep, host-tested and
// linked here unchanged. This file maps the traits onto esp-hal 1.2:
//
//   * `WakeKey`   : GPIO2 as pull-up input, `listen(LowLevel)` and
//                   `apply_wakeup_config(low_power_path = true)`. esp-hal 1.2
//                   has no wake-source list in the sleep call: a pin listening
//                   with its low-power path becomes an EXT1 level source at sleep
//                   entry (ESP32-C61 `sleep.ext1_version = 2`, GPIO0-6 are the
//                   LP pads, GPIO2 = LP_GPIO2). Sleep entry also copies the
//                   pull-up to the LP pad and holds the pad; the boot releases
//                   the hold (`wake_io_reset`). Without a listening LP pad
//                   `sleep_deep` PANICS ("no wakeup source"), which is why the
//                   sequence arms the wake before any irreversible step and
//                   `SleepEntry::enter` demands the `WakeArmed` proof.
//   * `EpdPark`   : `Epd::enter_deep_sleep` (DEEP_SLEEP mode 1, shared sequence).
//   * `SdFlush`   : `SdStorage::flush_and_close` (no CMD0: the card loses power).
//   * `C61Lines`  : `SpiControl::silence_lines` + PDM CLK (GPIO7) low.
//   * `C61SleepEntry` : `LowPower::sleep_deep(RtcSleepConfig::deep())`, never
//                   returns (`Entered = Infallible`).
//
// Not verified on hardware: that GPIO2 wakes the chip, sleep current, pad state
// of GPIO27 / GPIO10 / EPD DC during deep sleep (the BSP holds nothing, and
// neither does this port), EPD image retention, SD re-init after wake.

use core::convert::Infallible;

use esp_hal::{
    gpio::{Event, Input, InputConfig, Level, Output, OutputConfig, Pull, WakeupConfig},
    peripherals::{GPIO2, GPIO7, LPWR},
    rtc_cntl::{
        self, WakeupSource,
        sleep::{LowPower, RtcSleepConfig},
    },
};
use log::{error, info, warn};
use pulp_board_logic::power::PoweredOffProof;
use pulp_board_logic::sleep::{
    DisplayPark, LineSilencer, SdShutdown, SleepEntry, SleepReport, WAKE_SPEC, WakeArmed,
    WakeConfig, WakeError, WakeLevel, WakePull, WakeSpec,
};

pub use pulp_board_logic::sleep::{
    AbortReason, BootPlan, ChargeRestore, SleepAbort, SleepSequence, StepError, StoreSaver,
    WakeBits, WakeCause, classify_wake, plan_boot,
};

use super::epd::Epd;
use super::power::Gpio27Rail;
use super::spi::SpiControl;
use crate::drivers::sdcard::SdStorage;

/// Why the chip started, from esp-hal's wake record (empty after anything that
/// was not a deep-sleep wake). The ESP32-C61 low-power pad path reports as
/// `Ext1`; a digital pad wake would report as `Gpio`: both are the key.
pub fn wake_cause() -> WakeCause {
    let reason = rtc_cntl::wakeup_cause();
    let mut bits = WakeBits::default();
    for src in reason.iter() {
        match src {
            WakeupSource::Ext1 | WakeupSource::Gpio => bits.key_path = true,
            WakeupSource::Timer => bits.timer = true,
            _ => bits.other = true,
        }
    }
    classify_wake(bits)
}

/// GPIO2 deep-sleep wake (BSP `esp_sleep_enable_gpio_wakeup_on_hp_periph_powerdown(
/// GPIO2, LOW)`, board_c61.c:348-352). Holds its own `Input` on GPIO2 so the
/// listen / wake state outlives the call (the key driver owns the other
/// handle to the same pad, see `new`).
pub struct WakeKey {
    gpio: Option<GPIO2<'static>>,
    input: Option<Input<'static>>,
}

impl WakeKey {
    /// `gpio2` must be a second handle to the wake pad; the boot code makes it
    /// with `clone_unchecked` because the key driver (`keys::new`) owns the
    /// first one. Both configure the pad identically (pull-up input), and the
    /// key driver is not polled once the sleep sequence runs.
    pub fn new(gpio2: GPIO2<'static>) -> Self {
        Self {
            gpio: Some(gpio2),
            input: None,
        }
    }
}

impl WakeConfig for WakeKey {
    fn arm(&mut self, spec: &WakeSpec) -> Result<(), WakeError> {
        // the pad configuration below IS the BSP spec; refuse any other request
        // instead of arming something different
        if *spec != WAKE_SPEC || spec.level != WakeLevel::Low || spec.pull != WakePull::Up {
            return Err(WakeError::Other(
                "unsupported wake spec (only GPIO2 low, pull-up)",
            ));
        }
        if self.input.is_none()
            && let Some(g) = self.gpio.take()
        {
            self.input = Some(Input::new(g, InputConfig::default().with_pull(Pull::Up)));
        }
        let Some(input) = self.input.as_mut() else {
            return Err(WakeError::Other("wake pad unavailable"));
        };
        // key still down: the wake level is already present, so the chip would
        // wake at once (or sleep through it); let the caller retry after release
        if input.is_low() {
            return Err(WakeError::AlreadyAsserted);
        }
        // idempotent: repeating both calls leaves the same configuration
        input.listen(Event::LowLevel);
        input
            .apply_wakeup_config(&WakeupConfig::default().with_low_power_path(true))
            .map_err(|_| WakeError::NoLowPowerPath)?;
        info!("sleep: GPIO2 wake armed (low level, pull-up, low-power path)");
        Ok(())
    }
}

/// EPD controller into deep sleep mode 1.
pub struct EpdPark<'a>(pub &'a mut Epd);

impl DisplayPark for EpdPark<'_> {
    fn park(&mut self) -> Result<(), StepError> {
        match self.0.enter_deep_sleep() {
            Ok(()) => {
                info!("sleep: epd parked (deep sleep mode 1)");
                Ok(())
            }
            Err(e) => {
                warn!("sleep: epd park failed: {}", e.as_str());
                Err(StepError(e.as_str()))
            }
        }
    }
}

/// Flush and close FAT handles before the card loses power.
pub struct SdFlush<'a>(pub &'a SdStorage);

impl SdShutdown for SdFlush<'_> {
    fn shutdown(&mut self) -> Result<(), StepError> {
        self.0.flush_and_close();
        info!("sleep: sd flushed and closed");
        Ok(())
    }
}

/// SCK / MOSI / EPD CS / SD CS (through the SPI arbiter) and PDM CLK low.
pub struct C61Lines {
    spi: SpiControl,
    pdm_clk: Option<GPIO7<'static>>,
    pdm_out: Option<Output<'static>>,
}

impl C61Lines {
    pub fn new(spi: SpiControl, pdm_clk: GPIO7<'static>) -> Self {
        Self {
            spi,
            pdm_clk: Some(pdm_clk),
            pdm_out: None,
        }
    }
}

impl LineSilencer for C61Lines {
    fn silence(&mut self) -> Result<(), StepError> {
        let spi = self.spi.silence_lines();
        if self.pdm_out.is_none()
            && let Some(g) = self.pdm_clk.take()
        {
            self.pdm_out = Some(Output::new(g, Level::Low, OutputConfig::default()));
        }
        // PDM CLK is low from here on (the Output is created low and kept)
        match spi {
            Ok(()) => {
                info!("sleep: shared lines driven low");
                Ok(())
            }
            Err(e) => {
                warn!("sleep: line silencing incomplete: {:?}", e);
                Err(StepError("spi lines not silenced"))
            }
        }
    }
}

/// The last step. Holds LPWR; `enter` does not return.
pub struct C61SleepEntry {
    lpwr: Option<LPWR<'static>>,
}

impl C61SleepEntry {
    pub fn new(lpwr: LPWR<'static>) -> Self {
        Self { lpwr: Some(lpwr) }
    }

    /// Shared by the by-value and the by-`&mut` entry. Never returns.
    fn sleep(&mut self, wake: &WakeArmed, report: &SleepReport) -> Infallible {
        if report.saved() {
            info!("sleep: session saved before power-off");
        } else {
            warn!(
                "sleep: session NOT saved ({:?}); sleeping anyway",
                report.save
            );
        }
        info!(
            "sleep: entering deep sleep, wake = GPIO{} {:?}",
            wake.spec().gpio,
            wake.spec().level
        );
        // SAFETY (the `steal` fallback): `lpwr` is only `None` after a previous
        // `sleep`, which never returns, so the fallback cannot run twice and
        // nothing else holds LPWR (same reasoning as the X4 `enter_sleep`).
        let lpwr = match self.lpwr.take() {
            Some(l) => l,
            None => unsafe { LPWR::steal() },
        };
        // RtcSleepConfig::deep(): default deep sleep; nothing in RTC memory is
        // needed (the session is on the SD card)
        let mut low_power = LowPower::new(lpwr);
        low_power.sleep_deep(RtcSleepConfig::deep())
    }
}

impl SleepEntry for C61SleepEntry {
    type Entered = Infallible;

    fn enter(
        mut self,
        _rail_off: PoweredOffProof<'_>,
        wake: WakeArmed,
        report: &SleepReport,
    ) -> Infallible {
        self.sleep(&wake, report)
    }
}

// By `&mut`: the kernel keeps the parts across an aborted sequence (a held wake
// key, a rail in a state that cannot shut down) so the next idle timeout can
// try again; on success the chip sleeps and nothing is returned either way.
impl SleepEntry for &mut C61SleepEntry {
    type Entered = Infallible;

    fn enter(
        self,
        _rail_off: PoweredOffProof<'_>,
        wake: WakeArmed,
        report: &SleepReport,
    ) -> Infallible {
        self.sleep(&wake, report)
    }
}

impl WakeConfig for &mut WakeKey {
    fn arm(&mut self, spec: &WakeSpec) -> Result<(), WakeError> {
        (**self).arm(spec)
    }
}

impl LineSilencer for &mut C61Lines {
    fn silence(&mut self) -> Result<(), StepError> {
        (**self).silence()
    }
}

/// Run the full sequence on the C61. Returns only when the sequence aborted
/// before anything irreversible (wake could not be armed / rail not in a
/// shutdown-able state): the device is then still fully usable, and the abort
/// is logged. On success the chip sleeps and this call never returns.
///
/// Generic over how the parts are held: by value (the bring-up image, one try)
/// or by `&mut` (the kernel, which retries after an abort).
#[allow(clippy::too_many_arguments)]
pub fn enter_deep_sleep<Sv, Ch, Ln, Wk, En>(
    power: &mut pulp_board_logic::power::PeripheralPower<Gpio27Rail>,
    saver: Sv,
    epd: &mut Epd,
    sd: &SdStorage,
    charge: Ch,
    lines: Ln,
    wake: Wk,
    entry: En,
) -> SleepAbort
where
    Sv: pulp_board_logic::sleep::SessionSaver,
    Ch: ChargeRestore,
    Ln: LineSilencer,
    Wk: WakeConfig,
    En: SleepEntry<Entered = Infallible>,
{
    let seq = SleepSequence {
        power,
        saver,
        display: EpdPark(epd),
        sd: SdFlush(sd),
        charge,
        lines,
        wake,
        entry,
    };
    match seq.enter_deep_sleep() {
        Ok((_report, never)) => match never {},
        Err(abort) => {
            error!("sleep: aborted, staying awake: {:?}", abort.reason);
            abort
        }
    }
}
