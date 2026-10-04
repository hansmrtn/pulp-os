// SSD1677 e-paper on the OnePage C61 (R10, R11): esp-hal adapter only.
//
// Pins (BSP board_c61.c:39-42): CS25 (owned by the SPI arbiter, see `spi`),
// DC8 (:40, strapping pin, external pull-up), BUSY29 (:42), RST27 = the
// SD/MIC power rail (inside `PeripheralPower`, never reachable from here).
// All sequencing lives in pulp_board_logic::ssd1677 (host-tested): this file
// implements the three hardware traits and maps the failure type.
//
//   * `EpdBus`   : DC low + command byte / DC high + payload over the
//                  arbitrated EPD SPI device (CS handled per transaction).
//   * `DelayMs`  : blocking esp-hal `Delay`.
//   * `BusyPin`  : GPIO29 high = busy (BSP waits while level == 1, :147) and
//                  an `Instant`-based millisecond clock for the bounded wait.
//
// There is no reset-pin code on purpose: the driver resets with SW_RESET
// (0x12) and `Epd::init` needs the `DisplayReset` token from
// `PeripheralPower::display_reset()`, which is always `Software`.
//
// Not verified on hardware: DC level timing, SPI transfer of the 4000 B strips
// at 10 MHz over DMA, BUSY polarity/duration, waveform, ghosting, rotation.

use embedded_hal::spi::SpiDevice;
use esp_hal::{
    delay::Delay,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    peripherals::{GPIO8, GPIO29},
    time::Instant,
};
use pulp_board_logic::power::DelayMs;
use pulp_board_logic::ssd1677::{self, BusyPin, DisplayError, EpdBus};
use pulp_board_logic::strip::{StripCore, draw_bringup_pattern};

use super::spi::EpdSpiDevice;
use crate::error::{Error, ErrorKind};

pub use pulp_board_logic::ssd1677::{BUSY_TIMEOUT_MS, CoreStrips};

/// esp-hal side of the display: SPI device, DC, BUSY, delay.
pub struct EpdHw {
    spi: EpdSpiDevice,
    dc: Output<'static>,
    busy: Input<'static>,
    delay: Delay,
}

impl EpdBus for EpdHw {
    fn command(&mut self, c: u8) -> Result<(), DisplayError> {
        self.dc.set_low();
        let r = self.spi.write(&[c]);
        self.dc.set_high();
        r.map_err(|_| DisplayError::Bus)
    }

    fn data(&mut self, data: &[u8]) -> Result<(), DisplayError> {
        self.dc.set_high();
        self.spi.write(data).map_err(|_| DisplayError::Bus)
    }
}

impl DelayMs for EpdHw {
    fn delay_ms(&mut self, ms: u32) {
        self.delay.delay_millis(ms);
    }
}

impl BusyPin for EpdHw {
    fn is_busy(&mut self) -> bool {
        // BUSY_ACTIVE_HIGH: electrical high = busy
        self.busy.is_high()
    }

    fn now_ms(&mut self) -> u64 {
        Instant::now().duration_since_epoch().as_millis()
    }
}

pub type Epd = ssd1677::Epd<EpdHw>;

/// Build the display driver from the EPD SPI handle and the DC / BUSY pins
/// (`Pins::epd_dc`, `Pins::epd_busy`). Touches no GPIO27; the controller is
/// not initialised until `Epd::init`.
pub fn new(spi: EpdSpiDevice, dc: GPIO8<'static>, busy: GPIO29<'static>) -> Epd {
    let dc = Output::new(dc, Level::High, OutputConfig::default());
    let busy = Input::new(busy, InputConfig::default().with_pull(Pull::None));
    ssd1677::Epd::new(EpdHw {
        spi,
        dc,
        busy,
        delay: Delay::new(),
    })
}

/// Display failure as the kernel-wide error type. The X4 display path has no
/// error reporting at all (every EPD call is infallible and bus errors are
/// dropped), so there is no existing display error screen to reuse:
/// `ErrorKind` has no display variant (it is shared with X4 and left alone), so
/// this is `ErrorKind::Other` with the failure text as the source tag
/// (`Display` prints "error [display busy timeout]"). T12 decides how the app
/// layer shows it.
pub fn display_error(e: DisplayError) -> Error {
    Error::new(ErrorKind::Other, e.as_str())
}

/// One full refresh of the bring-up orientation card, through the same
/// strip -> RAM -> update -> bounded BUSY path a page render will use.
/// Blocking: returns within the BUSY limit even if the panel never answers.
pub fn full_refresh_test_pattern(epd: &mut Epd) -> Result<(), Error> {
    let mut core = StripCore::new();
    let mut src = CoreStrips {
        core: &mut core,
        draw: draw_bringup_pattern,
    };
    epd.full_refresh(&mut src).map_err(display_error)
}
