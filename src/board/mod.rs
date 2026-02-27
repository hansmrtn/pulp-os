// XTEink X4 board support package
//
// ESP32C3, SSD1677 800x480 epaper, SD card over shared SPI2 bus.
// ADC resistance ladders for buttons, GPIO3 power button with interrupt.
// SPI bus arbitrated via RefCellDevice (single threaded, no ISR access).

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
    gpio::{Event, Input, InputConfig, Io, Level, Output, OutputConfig, Pull},
    peripherals::{ADC1, GPIO0, GPIO1, GPIO2, Peripherals},
    spi,
    time::Rate,
};
use log::info;
use static_cell::StaticCell;

use crate::kernel::wake;

pub type SpiBus = spi::master::Spi<'static, Blocking>;
pub type SharedSpiDevice = RefCellDevice<'static, SpiBus, Output<'static>, Delay>;
pub type SdSpiDevice = RefCellDevice<'static, SpiBus, raw_gpio::RawOutputPin, Delay>;
pub type Epd = DisplayDriver<SharedSpiDevice, Output<'static>, Output<'static>, Input<'static>>;

static SPI_BUS: StaticCell<RefCell<SpiBus>> = StaticCell::new();

// power button lives in a static so the ISR can clear its interrupt
// and InputDriver can read pin level for debounce
static POWER_BTN: Mutex<RefCell<Option<Input<'static>>>> = Mutex::new(RefCell::new(None));

// shared GPIO ISR: handles power button (GPIO3) and display BUSY (GPIO6)
#[esp_hal::handler]
fn gpio_handler() {
    // power button -- pin in static, cleared via esp_hal API
    critical_section::with(|cs| {
        if let Some(btn) = POWER_BTN.borrow_ref_mut(cs).as_mut()
            && btn.is_interrupt_set()
        {
            btn.clear_interrupt();
            wake::signal_button();
        }
    });

    // display BUSY (GPIO6) -- owned by DisplayDriver, so check/clear
    // via raw register access since we cannot reach the pin from here
    const GPIO_STATUS: *const u32 = 0x6000_4044 as *const u32;
    const GPIO_STATUS_W1TC: *mut u32 = 0x6000_404C as *mut u32;
    const GPIO6_MASK: u32 = 1 << 6;

    unsafe {
        if GPIO_STATUS.read_volatile() & GPIO6_MASK != 0 {
            GPIO_STATUS_W1TC.write_volatile(GPIO6_MASK);
            wake::signal_display();
        }
    }
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

    // three phase SPI init: 400kHz bus -> SD probe -> speed up to 20MHz
    fn init_spi_peripherals(p: Peripherals) -> (DisplayHw, StorageHw) {
        let epd_cs = Output::new(p.GPIO21, Level::High, OutputConfig::default());
        let dc = Output::new(p.GPIO4, Level::High, OutputConfig::default());
        let rst = Output::new(p.GPIO5, Level::High, OutputConfig::default());

        // arm BUSY falling edge interrupt before handing pin to DisplayDriver;
        // the hardware config survives the move, ISR reads GPIO6 via registers
        let mut busy = Input::new(p.GPIO6, InputConfig::default().with_pull(Pull::None));
        busy.listen(Event::FallingEdge);
        info!("display BUSY: GPIO6 interrupt armed (FallingEdge)");

        // GPIO12 (flash SPIHD) is free in DIO mode but esp_hal does not
        // expose GPIO12-17 on ESP32C3, so we drive CS via raw registers
        let sd_cs = unsafe { raw_gpio::RawOutputPin::new(12) };

        let slow_cfg = spi::master::Config::default().with_frequency(Rate::from_khz(400));

        let mut spi_bus = spi::master::Spi::new(p.SPI2, slow_cfg)
            .unwrap()
            .with_sck(p.GPIO8)
            .with_mosi(p.GPIO10)
            .with_miso(p.GPIO7);

        // 10 bytes = 80 clocks with CS high (SD spec init requirement)
        let _ = spi_bus.write(&[0xFF; 10]);

        let spi_ref: &'static RefCell<SpiBus> = SPI_BUS.init(RefCell::new(spi_bus));

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
