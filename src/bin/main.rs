#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use esp_backtrace as _;
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::{
    gpio::{Input, InputConfig, Output, OutputConfig, Pull},
    peripherals::ADC1,
    spi::master::{Config as SpiConfig, Spi},
    time::Rate,
};
use log::info;
use ssd1677::{Builder, Dimensions, Display, Interface, RefreshMode, Region, Rotation, UpdateRegion};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

// ---- Button types ----

#[derive(Debug, Clone, Copy, PartialEq)]
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
            Button::Right => "Right",
            Button::Left => "Left",
            Button::Confirm => "Confirm",
            Button::Back => "Back",
            Button::VolUp => "Vol Up",
            Button::VolDown => "Vol Down",
            Button::Power => "Power",
        }
    }
}

// ---- Calibrated ADC thresholds (millivolts) ----
// Measured with AdcCalCurve.
// Idle reads ~2851 mV on both channels.

const TOLERANCE: u16 = 150;

/// Decode GPIO1 resistor ladder (Right, Left, Confirm, Back)
/// Values ordered low → high. Idle = ~2851.
fn decode_gpio1(mv: u16) -> Option<Button> {
    if in_range(mv, 3, 50)           { return Some(Button::Right); }
    if in_range(mv, 1113, TOLERANCE) { return Some(Button::Left); }
    if in_range(mv, 1984, TOLERANCE) { return Some(Button::Confirm); }
    if in_range(mv, 2556, TOLERANCE) { return Some(Button::Back); }
    None
}

/// Decode GPIO2 resistor ladder (Vol Down, Vol Up)
/// Values ordered low → high. Idle = ~2851.
fn decode_gpio2(mv: u16) -> Option<Button> {
    if in_range(mv, 3, 50)           { return Some(Button::VolDown); }
    if in_range(mv, 1659, TOLERANCE) { return Some(Button::VolUp); }
    None
}

fn in_range(val: u16, center: u16, tol: u16) -> bool {
    val >= center.saturating_sub(tol) && val <= center.saturating_add(tol)
}


#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    let delay = Delay::new();

    info!("pulp-os booting...");

    // --- ADC setup with calibration (blocking) ---
    let mut adc1_config = AdcConfig::new();
    let mut btn_row1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
        peripherals.GPIO1,
        Attenuation::_11dB,
    );
    let mut btn_row2_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
        peripherals.GPIO2,
        Attenuation::_11dB,
    );
    let mut adc1 = Adc::new(peripherals.ADC1, adc1_config);

    // Power button: digital, active LOW
    let power_btn = Input::new(
        peripherals.GPIO3,
        InputConfig::default().with_pull(Pull::Up),
    );

    let mut last_button: Option<Button> = None;

    // --- EPD pins ---
    let cs   = Output::new(peripherals.GPIO21, esp_hal::gpio::Level::High, OutputConfig::default());
    let dc   = Output::new(peripherals.GPIO4,  esp_hal::gpio::Level::Low,  OutputConfig::default());
    let rst  = Output::new(peripherals.GPIO5,  esp_hal::gpio::Level::High, OutputConfig::default());
    let busy = Input::new(peripherals.GPIO6, InputConfig::default().with_pull(Pull::None));

    // --- SPI ---
    let spi_cfg = SpiConfig::default()
        .with_frequency(Rate::from_mhz(10));
    let spi_bus = Spi::new(peripherals.SPI2, spi_cfg).unwrap()
        .with_sck(peripherals.GPIO8)
        .with_mosi(peripherals.GPIO10);

    let spi = embedded_hal_bus::spi::ExclusiveDevice::new(spi_bus, cs, Delay::new()).unwrap();

    // --- SSD1677 driver setup ---
    let interface = Interface::new(spi, dc, rst, busy);

    let dims = Dimensions::new(480, 800).unwrap();
    let cfg = Builder::new()
        .dimensions(dims)
        .rotation(Rotation::Rotate0)
        .build()
        .unwrap();

    let mut epd = Display::new(interface, cfg);

    let mut epd_delay = Delay::new();
    epd.reset(&mut epd_delay);

    // Full refresh 
    let region = Region::new(0, 0, 800, 480);
    let n = region.buffer_size();
    let bw = alloc::vec![0xFFu8; n]; // all white

    let update = UpdateRegion {
        region,
        black_buffer: &bw,
        red_buffer: &[],
        mode: RefreshMode::Full,
    };
    epd.update_region(update, &mut epd_delay);
    info!("refreshed display!");

    // Main event loop (kernel thing lol)
    loop {
        // Read button state (blocking ADC)
        let current = if power_btn.is_low() {
            Some(Button::Power)
        } else {
            let val1: u16 = nb::block!(adc1.read_oneshot(&mut btn_row1_pin)).unwrap();
            let val2: u16 = nb::block!(adc1.read_oneshot(&mut btn_row2_pin)).unwrap();
            decode_gpio1(val1).or_else(|| decode_gpio2(val2))
        };

        // Handle button transitions
        match (last_button, current) {
            (None, Some(btn)) => {
                info!("[BTN] {} pressed", btn.name());
            }
            (Some(btn), None) => {
                info!("[BTN] {} released", btn.name());
            }
            (Some(old), Some(new)) if old != new => {
                info!("[BTN] {} released", old.name());
                info!("[BTN] {} pressed", new.name());
            }
            _ => {}
        }
        last_button = current;

        // Poll delay (NOTE: will become WFI + timer interrupt later)
        delay.delay_millis(50);
    }
}
