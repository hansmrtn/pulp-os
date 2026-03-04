// board support for the XTEink X4 (ESP32-C3, SSD1677 800x480, SD over SPI2)
// DMA-backed SPI (GDMA CH0); CriticalSectionDevice arbitrates bus

pub mod action;
pub mod battery;
pub mod button;
pub mod layout;
pub mod raw_gpio;

pub use crate::drivers::sdcard::{SdStorage, SyncSdCard};
pub use crate::drivers::ssd1677::{DisplayDriver, HEIGHT, SPI_FREQ_MHZ, WIDTH};
pub use crate::drivers::strip::StripBuffer;
pub use button::{Button, ROW1_THRESHOLDS, ROW2_THRESHOLDS, decode_ladder};

pub const SCREEN_W: u16 = HEIGHT; // 480
pub const SCREEN_H: u16 = WIDTH; // 800

use core::cell::RefCell;

use critical_section::Mutex;
use embedded_hal_bus::spi::CriticalSectionDevice;
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

pub type SpiBus = spi::master::SpiDmaBus<'static, Blocking>;
pub type SharedSpiDevice = CriticalSectionDevice<'static, SpiBus, Output<'static>, Delay>;
pub type SdSpiDevice = CriticalSectionDevice<'static, SpiBus, raw_gpio::RawOutputPin, Delay>;
pub type Epd = DisplayDriver<SharedSpiDevice, Output<'static>, Output<'static>, Input<'static>>;

static SPI_BUS: StaticCell<Mutex<RefCell<SpiBus>>> = StaticCell::new();

// cached ref to the SPI bus mutex, set once in Board::init
// cached ref to the SPI bus mutex; pub(crate) so scheduler can
// access the bus in sd_card_sleep before deep sleep
pub(crate) static SPI_BUS_REF: Mutex<core::cell::Cell<Option<&'static Mutex<RefCell<SpiBus>>>>> =
    Mutex::new(core::cell::Cell::new(None));

// sd cs clone; only used in enter_sleep to send cmd0
// safety: same clone_unchecked pattern as gpio0/1/2/3 in init_input;
// only accessed after all normal sd i/o has stopped and before mcu halts
pub(crate) static SD_CS_SLEEP: Mutex<RefCell<Option<raw_gpio::RawOutputPin>>> =
    Mutex::new(RefCell::new(None));

static POWER_BTN: Mutex<RefCell<Option<Input<'static>>>> = Mutex::new(RefCell::new(None));

#[esp_hal::handler]
fn gpio_handler() {
    critical_section::with(|cs| {
        if let Some(btn) = POWER_BTN.borrow_ref_mut(cs).as_mut()
            && btn.is_interrupt_set()
        {
            btn.clear_interrupt();
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
    // sd card, initialised at 400 kHz before EPD touches the bus
    pub sd_card: Option<SyncSdCard>,
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

    // gpio / peripheral ownership:
    //
    // init_input (clone_unchecked)     init_spi_peripherals (move/clone)
    // ---                              ---
    // GPIO0   battery ADC              GPIO4   EPD DC
    // GPIO1   button row 1 ADC         GPIO5   EPD RST
    // GPIO2   button row 2 ADC         GPIO6   EPD BUSY
    // GPIO3   power button             GPIO7   SPI MISO
    // ADC1                             GPIO8   SPI SCK
    // IO_MUX                           GPIO10  SPI MOSI
    //                                  GPIO12  SD CS (raw register)
    //                                  GPIO21  EPD CS
    //                                  SPI2, DMA_CH0

    // Safety for all clone_unchecked calls below:
    //
    // init_input borrows Peripherals immutably and clones the pins it
    // needs.  init_spi_peripherals later takes ownership of the full
    // Peripherals struct but only touches a disjoint set of GPIOs
    // (GPIO4-8, GPIO10, GPIO21, SPI2, DMA_CH0).  See the ownership
    // table above for the complete split.  Each peripheral listed here
    // is used exclusively by InputHw and never touched again.
    fn init_input(p: &Peripherals) -> InputHw {
        let mut adc_cfg = AdcConfig::new();

        // Safety: GPIO1 is used only here (button row 1 ADC).
        let row1 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
            unsafe { p.GPIO1.clone_unchecked() },
            Attenuation::_11dB,
        );

        // Safety: GPIO2 is used only here (button row 2 ADC).
        let row2 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
            unsafe { p.GPIO2.clone_unchecked() },
            Attenuation::_11dB,
        );

        // Safety: GPIO0 is used only here (battery voltage ADC).
        let battery = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
            unsafe { p.GPIO0.clone_unchecked() },
            Attenuation::_11dB,
        );

        // Safety: ADC1 is used only here; init_spi_peripherals does not use ADC.
        let adc = Adc::new(unsafe { p.ADC1.clone_unchecked() }, adc_cfg);

        // Safety: IO_MUX is used only here for the GPIO interrupt handler.
        let mut io = Io::new(unsafe { p.IO_MUX.clone_unchecked() });
        io.set_interrupt_handler(gpio_handler);

        // Safety: GPIO3 is used only here (power button input with IRQ).
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

    // 400 kHz for SD probe, then 20 MHz; DMA-backed
    fn init_spi_peripherals(p: Peripherals) -> (DisplayHw, StorageHw) {
        let epd_cs = Output::new(p.GPIO21, Level::High, OutputConfig::default());
        let dc = Output::new(p.GPIO4, Level::High, OutputConfig::default());
        let rst = Output::new(p.GPIO5, Level::High, OutputConfig::default());
        let busy = Input::new(p.GPIO6, InputConfig::default().with_pull(Pull::None));

        // GPIO12 free in DIO mode; no esp-hal type, use raw registers
        let sd_cs = unsafe { raw_gpio::RawOutputPin::new(12) };

        // second handle to GPIO12 for sending cmd0 before deep sleep
        let sd_cs_sleep = unsafe { raw_gpio::RawOutputPin::new(12) };
        critical_section::with(|cs| {
            SD_CS_SLEEP.borrow_ref_mut(cs).replace(sd_cs_sleep);
        });

        let slow_cfg = spi::master::Config::default().with_frequency(Rate::from_khz(400));

        let mut spi_raw = spi::master::Spi::new(p.SPI2, slow_cfg)
            .unwrap()
            .with_sck(p.GPIO8)
            .with_mosi(p.GPIO10)
            .with_miso(p.GPIO7);

        // 80 clocks with CS high before DMA conversion (SD spec init)
        let _ = spi_raw.write(&[0xFF; 10]);

        // 4096B each direction: strip max ~4000B, SD sectors 512B
        let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = esp_hal::dma_buffers!(4096);
        let dma_rx_buf = DmaRxBuf::new(rx_descriptors, rx_buffer).unwrap();
        let dma_tx_buf = DmaTxBuf::new(tx_descriptors, tx_buffer).unwrap();

        let spi_dma_bus = spi_raw
            .with_dma(p.DMA_CH0)
            .with_buffers(dma_rx_buf, dma_tx_buf);

        let spi_ref: &'static Mutex<RefCell<SpiBus>> =
            SPI_BUS.init(Mutex::new(RefCell::new(spi_dma_bus)));
        info!("SPI bus: DMA enabled (CH0, 4096B TX+RX)");

        critical_section::with(|cs| SPI_BUS_REF.borrow(cs).set(Some(spi_ref)));

        let sd_spi = CriticalSectionDevice::new(spi_ref, sd_cs, Delay::new()).unwrap();

        // init SD card now, at 400 kHz on a pristine bus, before EPD
        // traffic -- SD spec requires CMD0 on a clean bus
        let sd_card = SdStorage::init_card(sd_spi);

        let epd_spi = CriticalSectionDevice::new(spi_ref, epd_cs, Delay::new()).unwrap();
        let epd = DisplayDriver::new(epd_spi, dc, rst, busy);

        (DisplayHw { epd }, StorageHw { sd_card })
    }
}

// switch SPI bus from 400 kHz to operational frequency (20 MHz)
// call after Board::init and before first EPD render
pub fn speed_up_spi() {
    let fast_cfg = spi::master::Config::default().with_frequency(Rate::from_mhz(SPI_FREQ_MHZ));
    critical_section::with(|cs| {
        if let Some(bus) = SPI_BUS_REF.borrow(cs).get() {
            bus.borrow(cs).borrow_mut().apply_config(&fast_cfg).unwrap();
            info!("SPI bus: 400kHz -> {}MHz", SPI_FREQ_MHZ);
        }
    });
}
