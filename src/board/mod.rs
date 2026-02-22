//! XTEink X4 Board Support Package (BSP)
//!
//! This module provides hardware abstraction for the XTEink X4 e-reader.
//! It maps physical hardware to named subsystems so that application code
//! doesn't need to know GPIO numbers or peripheral details.

pub mod button;
pub mod display;
pub mod pins;

pub use button::{decode_ladder, Button, ROW1_THRESHOLDS, ROW2_THRESHOLDS};
pub use display::{DisplayDriver, HEIGHT, WIDTH, FRAMEBUFFER_SIZE, SPI_FREQ_MHZ};

use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{
    analog::adc::{Adc, AdcCalCurve, AdcConfig, AdcPin, Attenuation},
    delay::Delay,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    peripherals::{Peripherals, ADC1, GPIO1, GPIO2},
    spi,
    time::Rate,
    Blocking,
};

// Type Aliases
pub type SpiBus = spi::master::Spi<'static, Blocking>;
pub type SpiDevice = ExclusiveDevice<SpiBus, Output<'static>, Delay>;
pub type Epd = DisplayDriver<SpiDevice, Output<'static>, Output<'static>, Input<'static>>;

// Hardware Bundles
/// Input subsystem hardware: ADC for button ladders + power button.
pub struct InputHw {
    pub adc: Adc<'static, ADC1<'static>, Blocking>,
    pub row1: AdcPin<GPIO1<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
    pub row2: AdcPin<GPIO2<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
    pub power: Input<'static>,
}

/// Display subsystem hardware: initialized e-paper display.
pub struct DisplayHw {
    pub epd: Epd,
}

/// Complete board hardware, ready for driver initialization.
pub struct Board {
    pub input: InputHw,
    pub display: DisplayHw,
}

impl Board {
    pub fn init(p: Peripherals) -> Self {
        let input = Self::init_input(&p);
        let display = Self::init_display(p);
        Board { input, display }
    }

    fn init_input(p: &Peripherals) -> InputHw {
        let mut adc_cfg = AdcConfig::new();

        // Configure both ADC channels with 11dB attenuation for full 0-3.3V range
        let row1 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
            unsafe { p.GPIO1.clone_unchecked() },
            Attenuation::_11dB,
        );

        let row2 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
            unsafe { p.GPIO2.clone_unchecked() },
            Attenuation::_11dB,
        );

        let adc = Adc::new(unsafe { p.ADC1.clone_unchecked() }, adc_cfg);

        let power = Input::new(
            unsafe { p.GPIO3.clone_unchecked() },
            InputConfig::default().with_pull(Pull::Up),
        );

        InputHw {
            adc,
            row1,
            row2,
            power,
        }
    }

    fn init_display(p: Peripherals) -> DisplayHw {
        // GPIO setup
        let cs = Output::new(p.GPIO21, Level::High, OutputConfig::default());
        let dc = Output::new(p.GPIO4, Level::High, OutputConfig::default());
        let rst = Output::new(p.GPIO5, Level::High, OutputConfig::default());
        let busy = Input::new(p.GPIO6, InputConfig::default().with_pull(Pull::None));

        // SPI bus
        let spi_cfg =
            spi::master::Config::default().with_frequency(Rate::from_mhz(SPI_FREQ_MHZ));
        let spi_bus = spi::master::Spi::new(p.SPI2, spi_cfg)
            .unwrap()
            .with_sck(p.GPIO8)
            .with_mosi(p.GPIO10);

        let spi_dev = ExclusiveDevice::new(spi_bus, cs, Delay::new()).unwrap();

        // Create display driver (our custom GxEPD2-based driver)
        let epd = DisplayDriver::new(spi_dev, dc, rst, busy);

        DisplayHw { epd }
    }
}
