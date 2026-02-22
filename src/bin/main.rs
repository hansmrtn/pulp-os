#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types"
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
    primitives::{Circle, Line, PrimitiveStyle, Rectangle},
    text::Text,
};

use pulp_os::board::Board;
use pulp_os::drivers::input::InputDriver;


extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// The rectangle that will flash on button events.
/// X must be 8-pixel aligned for efficient partial refresh.
const FLASH_RECT_X: u16 = 24;  // Aligned to 8 pixels
const FLASH_RECT_Y: u16 = 70;
const FLASH_RECT_W: u16 = 120; // Multiple of 8
const FLASH_RECT_H: u16 = 60;

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

    info!("booting pulp-os...");

    // ---- Hardware init ----
    let mut board = Board::init(peripherals);
    let mut delay = Delay::new();

    // Initialize display
    board.display.epd.init(&mut delay);
    
    let sz = board.display.epd.size();
    let w = sz.width as i32;
    let h = sz.height as i32;
    info!("display: {}x{}", w, h);

    // Fill framebuffer with white (no display update yet)
    board.display.epd.fill_white();

    // Draw initial content using embedded-graphics
    let black_stroke = PrimitiveStyle::with_stroke(BinaryColor::On, 2);
    let text_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);

    // 1. Border
    Rectangle::new(Point::new(2, 2), Size::new(sz.width - 4, sz.height - 4))
        .into_styled(black_stroke)
        .draw(&mut board.display.epd)
        .unwrap();

    // 2. Title
    Text::new("pulp-os", Point::new(20, 40), text_style)
        .draw(&mut board.display.epd)
        .unwrap();

    // 3. Crosshair at center
    let cx = w / 2;
    let cy = h / 2;
    Line::new(Point::new(cx - 20, cy), Point::new(cx + 20, cy))
        .into_styled(black_stroke)
        .draw(&mut board.display.epd)
        .unwrap();
    Line::new(Point::new(cx, cy - 20), Point::new(cx, cy + 20))
        .into_styled(black_stroke)
        .draw(&mut board.display.epd)
        .unwrap();

    // 4. The flash rectangle (starts black)
    let mut rect_is_black = true;
    Rectangle::new(
        Point::new(FLASH_RECT_X as i32, FLASH_RECT_Y as i32),
        Size::new(FLASH_RECT_W as u32, FLASH_RECT_H as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
    .draw(&mut board.display.epd)
    .unwrap();

    // 5. Circle
    Circle::new(Point::new(180, 70), 60)
        .into_styled(black_stroke)
        .draw(&mut board.display.epd)
        .unwrap();

    // 6. Instructions
    Text::new("display OK", Point::new(20, h - 20), text_style)
        .draw(&mut board.display.epd)
        .unwrap();

    // Initial full refresh
    board.display.epd.refresh_full(&mut delay);
    info!("Initial draw complete");

    // ---- Input polling and event loop ----
    let mut input = InputDriver::new(board.input);

    loop {
        if let Some(btn) = input.poll() {
            info!("[BTN] {:?} pressed", btn);
            
            // Toggle rectangle
            rect_is_black = !rect_is_black;
            let color = if rect_is_black {
                BinaryColor::On
            } else {
                BinaryColor::Off
            };

            // Draw to framebuffer
            Rectangle::new(
                Point::new(FLASH_RECT_X as i32, FLASH_RECT_Y as i32),
                Size::new(FLASH_RECT_W as u32, FLASH_RECT_H as u32),
            )
            .into_styled(PrimitiveStyle::with_fill(color))
            .draw(&mut board.display.epd)
            .unwrap();

            // Partial refresh - only the rectangle region
            board.display.epd.refresh_partial(
                FLASH_RECT_X,
                FLASH_RECT_Y,
                FLASH_RECT_W,
                FLASH_RECT_H,
                &mut delay,
            );

        }

        delay.delay_millis(10);
    }
}
