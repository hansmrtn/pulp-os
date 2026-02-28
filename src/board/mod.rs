// XTEink X4 board support package
//
// ESP32C3, SSD1677 800x480 epaper, SD over shared SPI2.
// DMA-backed SPI (GDMA CH0) — hardware pushes/pulls buffers over
// SPI autonomously, freeing the CPU from FIFO babysitting.
// RefCellDevice arbitrates bus (single threaded, no ISR access).
//
// Display BUSY (GPIO6) is no longer interrupt-driven here; esp-hal's
// async `Wait` implementation manages the GPIO6 interrupt internally
// when `Input::wait_for_low().await` is called via `block_on`.

pub mod action;
pub mod button;
pub mod pins;
pub mod raw_gpio;

pub use crate::drivers::sdcard::SdStorage;
pub use crate::drivers::ssd1677::{DisplayDriver, HEIGHT, SPI_FREQ_MHZ, WIDTH};
pub use crate::drivers::strip::StripBuffer;
pub use button::{Button, ROW1_THRESHOLDS, ROW2_THRESHOLDS, decode_ladder};

use core::cell::RefCell;

use critical_section::Mutex;
use embedded_hal_bus::spi::RefCellDevice;
use esp_hal::{
    Blocking,
    analog::adc::{Adc, AdcCalCurve, AdcConfig, AdcPin, Attenuation},
    delay::Delay,
    dma::{DmaRxBuf, DmaTxBuf},
    gpio::{Event, Input, InputConfig, Io, Level, Output, OutputConfig, Pull},
    peripherals::{ADC1, GPIO0, GPIO1, GPIO2, Peripherals},
    spi,
    time::Rate,
};
use log::info;
use static_cell::StaticCell;

use crate::kernel::wake;

pub type SpiBus = spi::master::SpiDmaBus<'static, Blocking>;
pub type SharedSpiDevice = RefCellDevice<'static, SpiBus, Output<'static>, Delay>;
pub type SdSpiDevice = RefCellDevice<'static, SpiBus, raw_gpio::RawOutputPin, Delay>;
pub type Epd = DisplayDriver<SharedSpiDevice, Output<'static>, Output<'static>, Input<'static>>;

static SPI_BUS: StaticCell<RefCell<SpiBus>> = StaticCell::new();

// power button static: ISR clears interrupt, InputDriver reads level
static POWER_BTN: Mutex<RefCell<Option<Input<'static>>>> = Mutex::new(RefCell::new(None));

// GPIO ISR: power button (GPIO3) only; BUSY (GPIO6) handled by esp-hal async Wait
#[esp_hal::handler]
fn gpio_handler() {
    critical_section::with(|cs| {
        if let Some(btn) = POWER_BTN.borrow_ref_mut(cs).as_mut()
            && btn.is_interrupt_set()
        {
            btn.clear_interrupt();
            wake::signal_button();
        }
    });
}

pub fn power_button_is_low() -> bool {
    critical_section::with(|cs| {
        POWER_BTN
            .borrow_ref_mut(cs)
            .as_mut()
            .map(|btn| btn.is_low())
            .unwrap_or(false)
    })
}

pub struct InputHw {
    pub adc: Adc<'static, ADC1<'static>, Blocking>,
    pub row1: AdcPin<GPIO1<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
    pub row2: AdcPin<GPIO2<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
    pub battery: AdcPin<GPIO0<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
}

pub struct DisplayHw {
    pub epd: Epd,
}

pub struct StorageHw {
    pub sd: SdStorage<SdSpiDevice>,
}

pub struct Board {
    pub input: InputHw,
    pub display: DisplayHw,
    pub storage: StorageHw,
}

impl Board {
    pub fn init(p: Peripherals) -> Self {
        let input = Self::init_input(&p);
        let (display, storage) = Self::init_spi_peripherals(p);
        Board {
            input,
            display,
            storage,
        }
    }

    fn init_input(p: &Peripherals) -> InputHw {
        let mut adc_cfg = AdcConfig::new();

        let row1 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
            unsafe { p.GPIO1.clone_unchecked() },
            Attenuation::_11dB,
        );

        let row2 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
            unsafe { p.GPIO2.clone_unchecked() },
            Attenuation::_11dB,
        );

        let battery = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
            unsafe { p.GPIO0.clone_unchecked() },
            Attenuation::_11dB,
        );

        let adc = Adc::new(unsafe { p.ADC1.clone_unchecked() }, adc_cfg);

        let mut io = Io::new(unsafe { p.IO_MUX.clone_unchecked() });
        io.set_interrupt_handler(gpio_handler);

        let mut power = Input::new(
            unsafe { p.GPIO3.clone_unchecked() },
            InputConfig::default().with_pull(Pull::Up),
        );
        power.listen(Event::FallingEdge);

        critical_section::with(|cs| {
            POWER_BTN.borrow_ref_mut(cs).replace(power);
        });
        info!("power button: GPIO3 interrupt armed (FallingEdge)");

        InputHw {
            adc,
            row1,
            row2,
            battery,
        }
    }

    // 400kHz -> SD probe -> 20MHz, DMA-backed
    fn init_spi_peripherals(p: Peripherals) -> (DisplayHw, StorageHw) {
        let epd_cs = Output::new(p.GPIO21, Level::High, OutputConfig::default());
        let dc = Output::new(p.GPIO4, Level::High, OutputConfig::default());
        let rst = Output::new(p.GPIO5, Level::High, OutputConfig::default());

        // do not arm an interrupt; esp-hal async Wait manages GPIO6 internally
        let busy = Input::new(p.GPIO6, InputConfig::default().with_pull(Pull::None));
        info!("display BUSY: GPIO6 (async wait, no pre-armed interrupt)");

        // GPIO12 free in DIO mode; esp_hal has no type, use raw registers
        let sd_cs = unsafe { raw_gpio::RawOutputPin::new(12) };

        let slow_cfg = spi::master::Config::default().with_frequency(Rate::from_khz(400));

        let mut spi_raw = spi::master::Spi::new(p.SPI2, slow_cfg)
            .unwrap()
            .with_sck(p.GPIO8)
            .with_mosi(p.GPIO10)
            .with_miso(p.GPIO7);

        // 80 clocks with CS high (SD spec init) — done on raw SPI
        // before DMA conversion since it's a one-shot init sequence.
        let _ = spi_raw.write(&[0xFF; 10]);

        // 4096B each direction; strip max ~4000B, SD sectors 512B
        let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = esp_hal::dma_buffers!(4096);
        let dma_rx_buf = DmaRxBuf::new(rx_descriptors, rx_buffer).unwrap();
        let dma_tx_buf = DmaTxBuf::new(tx_descriptors, tx_buffer).unwrap();

        let spi_dma_bus = spi_raw
            .with_dma(p.DMA_CH0)
            .with_buffers(dma_rx_buf, dma_tx_buf);

        let spi_ref: &'static RefCell<SpiBus> = SPI_BUS.init(RefCell::new(spi_dma_bus));
        info!("SPI bus: DMA enabled (CH0, 4096B TX+RX)");

        let sd_spi = RefCellDevice::new(spi_ref, sd_cs, Delay::new()).unwrap();
        let sd = SdStorage::new(sd_spi);

        let fast_cfg = spi::master::Config::default().with_frequency(Rate::from_mhz(SPI_FREQ_MHZ));
        spi_ref.borrow_mut().apply_config(&fast_cfg).unwrap();
        info!("SPI bus: 400kHz -> {}MHz", SPI_FREQ_MHZ);

        let epd_spi = RefCellDevice::new(spi_ref, epd_cs, Delay::new()).unwrap();
        let epd = DisplayDriver::new(epd_spi, dc, rst, busy);

        (DisplayHw { epd }, StorageHw { sd })
    }
}
