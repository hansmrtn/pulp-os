//! XTEink X4 BSP
//!
//! This module is to map the X4's physical hardware to named subsystems. 
//! All the pin assignments, bus configs, and calibration consts are here. 
//! Goal: have no need for another part of pulp-os to know GPIO
//!
//! Pin Map:
//! GPIO |     Function    |      Notes
//!  1   | ADC1 - Button 2 | Resistance ladder button for Right/Left/Confirm/Back
//!  2   | ADC2 - Button 1 | Resistance ladder (for consistency): Volume Up/Down
//!  3   | Digital - Power | Active LOW, internal pullup
//!  5   | EPD RST         | Reset (active low)
//!  6   | EPD BUSY        | Busy signal from display
//!  8   | SPI2 SCK        | Shared SPI clock
//! 10   | SPI2 MOSI       | Shared SPI data out
//! 21   | EPD CS          | Display chip select                    

use esp_hal::{
    analog::adc::{ Adc, AdcCalCurve, AdcPin, AdcConfig, Attenuation}, 
    gpio::{Output, Input, Pull, InputConfig, OutputConfig, Level },
    peripherals::{ Peripherals, ADC1, GPIO1, GPIO2 }, 
    time::Rate,
    delay::Delay,
    spi,
    Blocking,
};
use embedded_hal_bus::spi::ExclusiveDevice;
use ssd1677::{Interface, Display, Rotation, Builder, Dimensions};

// Display
pub const DISPLAY_WIDTH: u16 = 480;
pub const DISPLAY_HEIGHT: u16 = 800;

pub const FB_SIZE: usize = (DISPLAY_WIDTH as usize * DISPLAY_HEIGHT as usize) / 8;

// SPI clock rate for the epaper dispaly
pub const EPD_SPI_FREQ: u32 = 10; 

// Buttons
pub const BTN_TOLERANCE: u16 = 150; 

// Calibrated(?) tolerance band for the resistence ladder btns 
pub const ROW1_THRESHOLDS: &[(u16, u16, Button)] = &[
    // center (mV), move, button
    (3, 50, Button::Right), 
    (1113, BTN_TOLERANCE, Button::Left), 
    (1984, BTN_TOLERANCE, Button::Back), 
    (2556, BTN_TOLERANCE, Button::Confirm), 
]; 

pub const ROW2_THRESHOLDS: &[(u16, u16, Button)] = &[
    (3, 50, Button::VolDown), 
    (1659, BTN_TOLERANCE, Button::VolUp), 
]; 

pub type SpiBus = spi::master::Spi<'static, Blocking>;

pub type SpiDev = ExclusiveDevice<SpiBus, Output<'static>, Delay>;

pub type EpdInterface = Interface<SpiDev, Output<'static>, Output<'static>, Input<'static>>;

pub type Epd = Display<EpdInterface>;

pub type EpdCs       = Output<'static>;
pub type EpdDc       = Output<'static>;
pub type EpdRst      = Output<'static>;
pub type EpdBusy     = Input<'static>;
pub type PowerButton = Input<'static>;

// pub type AdcRow1 = AdcPin<P, ADC1<'static>, AdcCalCurve<ADC1<'static>>>;
// pub type AdcRow2 = AdcPin<P, ADC1<'static>, AdcCalCurve<ADC1<'static>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Right,
    Left,
    Confirm,
    Back,
    VolUp,
    VolDown,
    Power,
}

impl Button {
    pub fn name(self) -> &'static str {
        match self {
            Button::Right   => "Right",
            Button::Left    => "Left",
            Button::Confirm => "Confirm",
            Button::Back    => "Back",
            Button::VolUp   => "Vol Up",
            Button::VolDown => "Vol Down",
            Button::Power   => "Power",
        }
    }
}

impl core::fmt::Display for Button {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

pub struct InputHw {
    pub adc:   Adc<'static, ADC1<'static>, Blocking>,
    pub row1:  AdcPin<GPIO1<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
    pub row2:  AdcPin<GPIO2<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
    pub power: Input<'static>,
}

pub struct DisplayHw {
    pub epd: Epd,   // type alias chain from before
}

pub struct Board {
    pub input: InputHw,
    pub display: DisplayHw,
}

impl Board{
    pub fn init(p: Peripherals) -> Self {
        let mut adc_cfg = AdcConfig::new(); 

        let row1 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
                p.GPIO1,
                Attenuation::_11dB,
        );
        
        let row2 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
                p.GPIO2,
                Attenuation::_11dB,
        );

        let adc = Adc::new(p.ADC1, adc_cfg); 

        let power = Input::new(p.GPIO3, InputConfig::default().with_pull(Pull::Up));

        let input = InputHw { adc, row1, row2, power }; 

        // Display: SPI bus + EPD
        let cs = Output::new(p.GPIO21,Level::High, OutputConfig::default());
        let dc= Output::new(p.GPIO4,Level::High, OutputConfig::default());
        let rst= Output::new(p.GPIO5,Level::High, OutputConfig::default());
        let busy= Input::new(p.GPIO6, InputConfig::default().with_pull(Pull::None)); 

        let spi_cfg = spi::master::Config::default().with_frequency(Rate::from_mhz(EPD_SPI_FREQ));
        let spi_bus = spi::master::Spi::new(p.SPI2, spi_cfg).unwrap().with_sck(p.GPIO8).with_mosi(p.GPIO10);

        let spi_dev = ExclusiveDevice::new(spi_bus, cs, Delay::new()).unwrap();

        let interface = Interface::new(spi_dev, dc, rst, busy);

        let dims = Dimensions::new(DISPLAY_WIDTH, DISPLAY_HEIGHT).unwrap();
        let cfg = Builder::new()
            .dimensions(dims)
            .rotation(Rotation::Rotate0)
            .build()
            .unwrap();

        let mut epd = Display::new(interface, cfg);
        epd.reset(&mut Delay::new());

        let display = DisplayHw { epd };

        Board { input, display }
    }
}


/// Decode a millivolt reading against a threshold table.
/// Used by the input subsystem to map ADC readings to buttons.
pub fn decode_ladder(mv: u16, thresholds: &[(u16, u16, Button)]) -> Option<Button> {
    for &(center, tol, button) in thresholds {
        if mv >= center.saturating_sub(tol) && mv <= center.saturating_add(tol) {
            return Some(button);
        }
    }
    None
}
