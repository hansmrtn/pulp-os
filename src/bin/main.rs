#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
// use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
use esp_hal::clock::CpuClock;
// use esp_hal::gpio::{Input, InputConfig, Pull};
// use esp_hal::peripherals::ADC1;
use esp_hal::timer::timg::TimerGroup;
use log::info;

use embassy_time::{Delay, Duration, Timer};
use esp_hal::{
    gpio::{Input, InputConfig, Output, OutputConfig, Pull},
    peripherals::ADC1,
    spi::master::{Config as SpiConfig, Spi},
    time::Rate,
};
use ssd1677::{
    Builder, Dimensions, Display, Interface, RefreshMode, Region, Rotation, UpdateRegion,
};

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
    if in_range(mv, 3, 50) {
        return Some(Button::Right);
    }
    if in_range(mv, 1113, TOLERANCE) {
        return Some(Button::Left);
    }
    if in_range(mv, 1984, TOLERANCE) {
        return Some(Button::Confirm);
    }
    if in_range(mv, 2556, TOLERANCE) {
        return Some(Button::Back);
    }
    None
}

/// Decode GPIO2 resistor ladder (Vol Down, Vol Up)
/// Values ordered low → high. Idle = ~2851.
fn decode_gpio2(mv: u16) -> Option<Button> {
    if in_range(mv, 3, 50) {
        return Some(Button::VolDown);
    }
    if in_range(mv, 1659, TOLERANCE) {
        return Some(Button::VolUp);
    }
    None
}

fn in_range(val: u16, center: u16, tol: u16) -> bool {
    val >= center.saturating_sub(tol) && val <= center.saturating_add(tol)
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    info!("pulp-os booting...");

    // --- ADC setup with calibration ---
    let mut adc1_config = AdcConfig::new();
    let mut btn_row1_pin = adc1_config
        .enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(peripherals.GPIO1, Attenuation::_11dB);
    let mut btn_row2_pin = adc1_config
        .enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(peripherals.GPIO2, Attenuation::_11dB);
    let mut adc1 = Adc::new(peripherals.ADC1, adc1_config).into_async();

    // Power button: digital, active LOW
    let power_btn = Input::new(
        peripherals.GPIO3,
        InputConfig::default().with_pull(Pull::Up),
    );

    let _ = spawner;

    let mut last_button: Option<Button> = None;

    let config = esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::max());
    // let p = esp_hal::init(config);

    // --- EPD pins (Xteink X4 reference) ---
    // CS=21, DC=4, RST=5, BUSY=6; SCLK=8, MOSI=10 :contentReference[oaicite:8]{index=8}
    let cs = Output::new(
        peripherals.GPIO21,
        esp_hal::gpio::Level::High,
        OutputConfig::default(),
    );
    let dc = Output::new(
        peripherals.GPIO4,
        esp_hal::gpio::Level::Low,
        OutputConfig::default(),
    );
    let rst = Output::new(
        peripherals.GPIO5,
        esp_hal::gpio::Level::High,
        OutputConfig::default(),
    );
    let busy = Input::new(
        peripherals.GPIO6,
        InputConfig::default().with_pull(Pull::None),
    );

    // --- SPI (write-only is fine for most EPD flows) ---
    // Pick the correct SPI peripheral for your chip (ESP32-C3 commonly uses SPI2).
    let spi_cfg = SpiConfig::default().with_frequency(Rate::from_mhz(10)); // start conservative; you can raise later
    let spi_bus = Spi::new(peripherals.SPI2, spi_cfg)
        .unwrap()
        .with_sck(peripherals.GPIO8)
        .with_mosi(peripherals.GPIO10);

    let mut spi = embedded_hal_bus::spi::ExclusiveDevice::new(spi_bus, cs, Delay).unwrap();

    // --- SSD1677 driver setup ---
    let interface = Interface::new(spi, dc, rst, busy);

    // Dimensions::new(rows, cols) == (height, width) per driver README. :contentReference[oaicite:10]{index=10}
    let dims = Dimensions::new(480, 800).unwrap();
    let cfg = Builder::new()
        .dimensions(dims)
        .rotation(Rotation::Rotate0)
        .build()
        .unwrap();

    let mut epd = Display::new(interface, cfg);

    // Blocking delay object that satisfies embedded-hal DelayNs. :contentReference[oaicite:11]{index=11}
    let mut delay = Delay;

    epd.reset(&mut delay);

    // --- Memory-friendly: update a small region ---
    // Region x and w should be multiples of 8 pixels (byte aligned). :contentReference[oaicite:12]{index=12}
    let region = Region::new(0, 0, 800, 480); // 200 must be multiple of 8; change as needed
    let n = region.buffer_size(); // compute required bytes :contentReference[oaicite:13]{index=13}

    let mut bw = alloc::vec![0xFFu8; n]; // all white (1=white, 0=black) :contentReference[oaicite:14]{index=14}
    // draw something by clearing bits in `bw`...

    let update = UpdateRegion {
        region,
        black_buffer: &bw,
        red_buffer: &[], // disables red plane :contentReference[oaicite:15]{index=15}
        mode: RefreshMode::Full, // or Full / Partial :contentReference[oaicite:16]{index=16}
    };

    epd.update_region(update, &mut delay);

    info!("refreshed display!");

    loop {
        // Read current button state
        let current = if power_btn.is_low() {
            Some(Button::Power)
        } else {
            let val1: u16 = adc1.read_oneshot(&mut btn_row1_pin).await;
            let val2: u16 = adc1.read_oneshot(&mut btn_row2_pin).await;
            decode_gpio1(val1).or_else(|| decode_gpio2(val2))
        };

        // Log transitions only
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
        Timer::after(Duration::from_millis(50)).await;
    }
}
