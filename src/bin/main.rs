#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::analog::adc::{Adc, AdcCalCurve, AdcConfig, Attenuation};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::peripherals::ADC1;
use esp_hal::timer::timg::TimerGroup;
use log::info;

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
    if in_range(mv, 3, 50)         { return Some(Button::Right); }
    if in_range(mv, 1113, TOLERANCE) { return Some(Button::Left); }
    if in_range(mv, 1984, TOLERANCE) { return Some(Button::Confirm); }
    if in_range(mv, 2556, TOLERANCE) { return Some(Button::Back); }
    None
}

/// Decode GPIO2 resistor ladder (Vol Down, Vol Up)
/// Values ordered low → high. Idle = ~2851.
fn decode_gpio2(mv: u16) -> Option<Button> {
    if in_range(mv, 3, 50)         { return Some(Button::VolDown); }
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
    let mut btn_row1_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
        peripherals.GPIO1,
        Attenuation::_11dB,
    );
    let mut btn_row2_pin = adc1_config.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
        peripherals.GPIO2,
        Attenuation::_11dB,
    );
    let mut adc1 = Adc::new(peripherals.ADC1, adc1_config).into_async();

    // Power button: digital, active LOW
    let power_btn = Input::new(
        peripherals.GPIO3,
        InputConfig::default().with_pull(Pull::Up),
    );

    let _ = spawner;

    let mut last_button: Option<Button> = None;

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
