#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use log::info;
use ssd1677::{RefreshMode, Region, UpdateRegion};

use pulp_os::board::{self, Board, Button, ROW1_THRESHOLDS, ROW2_THRESHOLDS};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

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

    info!("pulp-os booting...");

    // ---- Hardware init ----
    let Board { mut input, mut display } = Board::init(peripherals);

    // ---- Initial display: blank white ----
    let region = Region::new(0, 0, board::DISPLAY_HEIGHT, board::DISPLAY_WIDTH);
    let n = region.buffer_size();
    let bw = alloc::vec![0xFFu8; n];

    let update = UpdateRegion {
        region,
        black_buffer: &bw,
        red_buffer: &[],
        mode: RefreshMode::Full,
    };
    display.epd.update_region(update, &mut Delay::new());
    info!("display ready");

    // ---- Event loop ----
    let delay = Delay::new();
    let mut last_button: Option<Button> = None;

    loop {
        // Read current button state
        let current = if input.power.is_low() {
            Some(Button::Power)
        } else {
            let mv1: u16 = nb::block!(input.adc.read_oneshot(&mut input.row1)).unwrap();
            let mv2: u16 = nb::block!(input.adc.read_oneshot(&mut input.row2)).unwrap();
            board::decode_ladder(mv1, ROW1_THRESHOLDS)
                .or_else(|| board::decode_ladder(mv2, ROW2_THRESHOLDS))
        };

        // Log transitions
        match (last_button, current) {
            (None, Some(btn)) => {
                info!("[BTN] {} pressed", btn);
            }
            (Some(btn), None) => {
                info!("[BTN] {} released", btn);
            }
            (Some(old), Some(new)) if old != new => {
                info!("[BTN] {} released, {} pressed", old, new);
            }
            _ => {}
        }
        last_button = current;

        delay.delay_millis(50);
    }
}
