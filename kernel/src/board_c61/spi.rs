// SPI2 + DMA for the OnePage C61 (R8). EPD and SD share the bus.
//
// Wiring (BSP board_c61.c): SCK22 :38, MOSI23 :37, MISO24 :96, EPD CS25 :39,
// SD CS26 :58. Pin numbers/timing live in pulp_board_logic (host-tested);
// this file only maps them onto esp-hal 1.2 (`Spi` -> `SpiDma`, DMA_CH0,
// `dma_rx_buffer!`/`dma_tx_buffer!`, same pattern as the X4 port).
//
// Exclusion: the bus sits in a critical-section mutex; every device access is
// one `SpiArbiter::transaction` (assert CS, run ops, flush, release CS) from
// pulp_board_logic::spi, so there is no other way to move a chip select. CS
// lines are plain esp-hal `Output`s: no raw GPIO registers (the X4
// `board::raw_gpio` is C3-only and is not compiled for the C61).
//
// Not verified on hardware: SPI clocks, DMA transfers, MISO pull-up. NOTE the
// BSP enables the MISO internal pull-up (:89); esp-hal's `with_miso` resets the
// pad config to no-pull, so it is not applied here (SD DO relies on the card
// / board pull-up). Revisit at bring-up if init is flaky.

use core::cell::RefCell;

use critical_section::Mutex;
use embedded_hal::delay::DelayNs;
use embedded_hal::spi::{ErrorKind, ErrorType, Operation, SpiDevice};
use esp_hal::{
    Blocking,
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    peripherals::{DMA_CH0, GPIO22, GPIO23, GPIO24, GPIO25, GPIO26, SPI2},
    spi::{self, master::SpiDma},
    time::Rate,
};
use log::info;
use pulp_board_logic::spi::{
    BusError, ChipSelect, Device, SD_PRELUDE_BYTES, SPI_DMA_BUF_BYTES, SPI_INIT_KHZ,
    SPI_OPERATING_KHZ, SpiArbiter,
};
use static_cell::StaticCell;

pub type SpiBus = SpiDma<'static, Blocking>;

/// Active-low chip select on an esp-hal output (low = selected).
pub struct CsOutput(Output<'static>);

impl ChipSelect for CsOutput {
    fn assert(&mut self) {
        self.0.set_low();
    }
    fn release(&mut self) {
        self.0.set_high();
    }
}

struct SharedSpi {
    bus: SpiBus,
    arbiter: SpiArbiter<CsOutput, CsOutput>,
}

type Shared = Mutex<RefCell<SharedSpi>>;

static SPI_SHARED: StaticCell<Shared> = StaticCell::new();

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SpiDeviceError {
    /// Another transaction owns the bus (or the bus cell is already borrowed).
    Busy,
    /// The underlying SPI transfer failed (CS was released).
    Bus,
}

impl embedded_hal::spi::Error for SpiDeviceError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// One device on the shared bus. Cheap handle (shared ref + which device):
/// cloning does not duplicate the chip select, which the arbiter owns.
#[derive(Clone)]
pub struct ArbitratedSpiDevice {
    shared: &'static Shared,
    dev: Device,
    delay: Delay,
}

pub type SdSpiDevice = ArbitratedSpiDevice;
pub type EpdSpiDevice = ArbitratedSpiDevice;

impl ErrorType for ArbitratedSpiDevice {
    type Error = SpiDeviceError;
}

fn run_ops(
    bus: &mut SpiBus,
    delay: &mut Delay,
    ops: &mut [Operation<'_, u8>],
) -> Result<(), SpiDeviceError> {
    let mut res: Result<(), SpiDeviceError> = Ok(());
    for op in ops.iter_mut() {
        let r = match op {
            Operation::Read(buf) => embedded_hal::spi::SpiBus::read(bus, buf),
            Operation::Write(buf) => embedded_hal::spi::SpiBus::write(bus, buf),
            Operation::Transfer(read, write) => {
                embedded_hal::spi::SpiBus::transfer(bus, read, write)
            }
            Operation::TransferInPlace(buf) => {
                embedded_hal::spi::SpiBus::transfer_in_place(bus, buf)
            }
            Operation::DelayNs(ns) => {
                let f = embedded_hal::spi::SpiBus::flush(bus);
                if f.is_ok() {
                    delay.delay_ns(*ns);
                }
                f
            }
        };
        if r.is_err() {
            res = Err(SpiDeviceError::Bus);
            break;
        }
    }
    // always flush before CS is released, also after a failed op
    if embedded_hal::spi::SpiBus::flush(bus).is_err() {
        res = Err(SpiDeviceError::Bus);
    }
    res
}

impl SpiDevice for ArbitratedSpiDevice {
    fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), SpiDeviceError> {
        let shared = self.shared;
        let dev = self.dev;
        let delay = &mut self.delay;
        critical_section::with(|cs| {
            let mut guard = shared
                .borrow(cs)
                .try_borrow_mut()
                .map_err(|_| SpiDeviceError::Busy)?;
            let SharedSpi { bus, arbiter } = &mut *guard;
            arbiter
                .transaction(dev, || run_ops(bus, delay, operations))
                .map_err(|e| match e {
                    BusError::Busy(_) => SpiDeviceError::Busy,
                    BusError::Transfer(e) => e,
                })
        })
    }
}

/// Bus pins and CS lines, taken from `board_c61::Pins`.
pub struct SpiPins {
    pub sck: GPIO22<'static>,
    pub mosi: GPIO23<'static>,
    pub miso: GPIO24<'static>,
    pub epd_cs: GPIO25<'static>,
    pub sd_cs: GPIO26<'static>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SpiInitError {
    /// `Spi::new` / `apply_config` rejected the configuration.
    Config,
    /// DMA descriptor/buffer allocation failed.
    DmaBuffers,
    /// Bus cell busy while changing the clock.
    Busy,
}

/// Switch the bus clock (init 400 kHz -> operating 10 MHz).
#[derive(Clone)]
pub struct SpiControl {
    shared: &'static Shared,
}

impl SpiControl {
    /// Call after SD init (needs 400 kHz) and before the first EPD frame.
    pub fn speed_up(&self) -> Result<(), SpiInitError> {
        self.set_khz(SPI_OPERATING_KHZ, SPI_INIT_KHZ)
    }

    /// Back to the SD probe clock before a card inserted at runtime is
    /// initialised (T12); `speed_up` again afterwards.
    pub fn slow_down(&self) -> Result<(), SpiInitError> {
        self.set_khz(SPI_INIT_KHZ, SPI_OPERATING_KHZ)
    }

    fn set_khz(&self, to: u32, from: u32) -> Result<(), SpiInitError> {
        let cfg = spi::master::Config::default().with_frequency(Rate::from_khz(to));
        let r = critical_section::with(|cs| {
            let mut g = self
                .shared
                .borrow(cs)
                .try_borrow_mut()
                .map_err(|_| SpiInitError::Busy)?;
            g.bus.apply_config(&cfg).map_err(|_| SpiInitError::Config)
        });
        if r.is_ok() {
            info!("spi2: {} kHz -> {} kHz", from, to);
        }
        r
    }
}

impl SpiControl {
    /// Shutdown only (BSP `board_sleep_enter`, board_c61.c:340-343): leave SCK,
    /// MOSI and both chip selects low so they cannot back-power the rail that
    /// is about to be cut. esp-hal gives the SCK/MOSI pads to the SPI driver, so
    /// they cannot be driven as plain outputs from here: SCK idles low in SPI
    /// mode 0, and one 0x00 byte (no CS asserted, both devices deselected) leaves
    /// MOSI low; then both CS outputs are driven low through the arbiter. Not
    /// verified on hardware: that this is enough to keep the pads from
    /// back-powering the rail (the BSP's `gpio_set_level` on SPI-routed pads is
    /// itself of doubtful effect).
    pub fn silence_lines(&self) -> Result<(), SpiDeviceError> {
        critical_section::with(|cs| {
            let mut g = self
                .shared
                .borrow(cs)
                .try_borrow_mut()
                .map_err(|_| SpiDeviceError::Busy)?;
            let SharedSpi { bus, arbiter } = &mut *g;
            let sent = embedded_hal::spi::SpiBus::write(bus, &[0x00])
                .and_then(|_| embedded_hal::spi::SpiBus::flush(bus))
                .map_err(|_| SpiDeviceError::Bus);
            // CS low regardless of the byte: this is the last thing the bus does
            let cs_low = arbiter.park_selects_low().map_err(|_| SpiDeviceError::Busy);
            sent.and(cs_low)
        })
    }
}

pub struct SpiBoard {
    /// EPD device handle (T6 builds the SSD1677 driver on it; DC/BUSY and the
    /// software reset stay in `Pins`).
    pub epd: EpdSpiDevice,
    pub sd: SdSpiDevice,
    pub control: SpiControl,
}

/// Bring up SPI2 + DMA at the SD probe clock. Consumes SPI2/DMA_CH0 and the
/// pins, so it can only run once (the DMA buffer statics are created here).
pub fn init(
    spi2: SPI2<'static>,
    dma: DMA_CH0<'static>,
    pins: SpiPins,
) -> Result<SpiBoard, SpiInitError> {
    // both CS high before any clock edge
    let epd_cs = Output::new(pins.epd_cs, Level::High, OutputConfig::default());
    let sd_cs = Output::new(pins.sd_cs, Level::High, OutputConfig::default());

    let slow = spi::master::Config::default().with_frequency(Rate::from_khz(SPI_INIT_KHZ));
    let mut raw = spi::master::Spi::new(spi2, slow)
        .map_err(|_| SpiInitError::Config)?
        .with_sck(pins.sck)
        .with_mosi(pins.mosi)
        .with_miso(pins.miso);

    // SD spec: >= 74 clocks with CS high before CMD0 (BSP gets this from the
    // IDF sdspi host). Same trick as the X4 port, before the DMA conversion.
    // A failure here is not fatal: the SD init retry loop recovers.
    let _ = raw.write(&[0xFF; SD_PRELUDE_BYTES]);

    let rx = esp_hal::dma_rx_buffer!(SPI_DMA_BUF_BYTES).map_err(|_| SpiInitError::DmaBuffers)?;
    let tx = esp_hal::dma_tx_buffer!(SPI_DMA_BUF_BYTES).map_err(|_| SpiInitError::DmaBuffers)?;
    let bus = raw.with_dma(dma).with_buffers(rx, tx);

    let arbiter = SpiArbiter::new(CsOutput(epd_cs), CsOutput(sd_cs));
    let shared: &'static Shared =
        SPI_SHARED.init(Mutex::new(RefCell::new(SharedSpi { bus, arbiter })));
    info!(
        "spi2: sck=GPIO22 mosi=GPIO23 miso=GPIO24, epd_cs=GPIO25 sd_cs=GPIO26, dma ch0 {}B tx+rx, {} kHz",
        SPI_DMA_BUF_BYTES, SPI_INIT_KHZ
    );

    let delay = Delay::new();
    Ok(SpiBoard {
        epd: ArbitratedSpiDevice {
            shared,
            dev: Device::Epd,
            delay,
        },
        sd: ArbitratedSpiDevice {
            shared,
            dev: Device::Sd,
            delay,
        },
        control: SpiControl { shared },
    })
}
