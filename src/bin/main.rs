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

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle, Line},
    text::Text,
};

use pulp_os::board::Board;
use pulp_os::display::DisplayDriver;
use pulp_os::input::{Event, InputDriver};

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
    let Board { input, display } = Board::init(peripherals);
    let mut input = InputDriver::new(input);
    let mut display = DisplayDriver::new(display);

    // ---- Draw test pattern ----
    let sz = display.size();
    let w = sz.width as i32;
    let h = sz.height as i32;
    info!("display: {}x{}", w, h);

    display.clear_white();

    let black = PrimitiveStyle::with_stroke(BinaryColor::On, 2);
    let filled = PrimitiveStyle::with_fill(BinaryColor::On);
    let text_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);

    // 1. Border
    Rectangle::new(Point::new(2, 2), Size::new(sz.width - 4, sz.height - 4))
        .into_styled(black)
        .draw(&mut display)
        .unwrap();

    // 2. Title
    Text::new("pulp-os", Point::new(20, 40), text_style)
        .draw(&mut display)
        .unwrap();

    // 3. Crosshair at center
    let cx = w / 2;
    let cy = h / 2;
    Line::new(Point::new(cx - 20, cy), Point::new(cx + 20, cy))
        .into_styled(black)
        .draw(&mut display)
        .unwrap();
    Line::new(Point::new(cx, cy - 20), Point::new(cx, cy + 20))
        .into_styled(black)
        .draw(&mut display)
        .unwrap();

    // 4. Filled rectangle
    Rectangle::new(Point::new(20, 70), Size::new(120, 60))
        .into_styled(filled)
        .draw(&mut display)
        .unwrap();

    // 5. Circle
    Circle::new(Point::new(180, 70), 60)
        .into_styled(black)
        .draw(&mut display)
        .unwrap();

    // 6. Label at bottom
    Text::new("draw test OK", Point::new(20, h - 20), text_style)
        .draw(&mut display)
        .unwrap();

    display.flush_full(&mut Delay::new());
    info!("draw test flushed");

    // ---- Event loop ----
    let delay = Delay::new();

    loop {
        while let Some(ev) = input.poll() {
            match ev {
                Event::Press(btn) => info!("[BTN] {} pressed", btn),
                Event::Release(btn) => info!("[BTN] {} released", btn),
                Event::LongPress(btn) => info!("[BTN] {} long-press", btn),
                Event::Repeat(btn) => info!("[BTN] {} repeat", btn),
            }
        }

        delay.delay_millis(20);
    }
}
