#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use log::info;

use embedded_graphics::mono_font::ascii::FONT_10X20;

use pulp_os::board::Board;
use pulp_os::drivers::input::{InputDriver, Event};
use pulp_os::ui::{Region, Widget, Label, Button};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

// 8px aligned
const TITLE: Region = Region::new(16, 16, 200, 32);
const BTN: Region = Region::new(16, 80, 120, 48);
const STATUS: Region = Region::new(16, 160, 200, 32);

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    info!("booting...");

    let mut board = Board::init(peripherals);
    let mut delay = Delay::new();

    board.display.epd.init(&mut delay);
    board.display.epd.fill_white();

    // test widgets
    let title = Label::new(TITLE, "pulp-os", &FONT_10X20);
    let mut btn = Button::new(BTN, "Press", &FONT_10X20);
    let mut status = Label::new(STATUS, "Ready", &FONT_10X20);

    // init draw
    title.draw(&mut board.display.epd).unwrap();
    btn.draw(&mut board.display.epd).unwrap();
    status.draw(&mut board.display.epd).unwrap();
    board.display.epd.refresh_full(&mut delay);

    info!("UI ready");

    let mut input = InputDriver::new(board.input);

    loop {
        if let Some(event) = input.poll() {
            match event {
                Event::Press(button) => {
                    info!("[BTN] Press: {}", button.name());

                    btn.set_pressed(true);
                    btn.draw(&mut board.display.epd).unwrap();
                    let r = btn.refresh_bounds();
                    board.display.epd.refresh_partial(r.x, r.y, r.w, r.h, &mut delay);

                    status.set_text(button.name());
                    status.draw(&mut board.display.epd).unwrap();
                    let r = status.refresh_bounds();
                    board.display.epd.refresh_partial(r.x, r.y, r.w, r.h, &mut delay);
                }
                Event::Release(_button) => {
                    btn.set_pressed(false);
                    btn.draw(&mut board.display.epd).unwrap();
                    let r = btn.refresh_bounds();
                    board.display.epd.refresh_partial(r.x, r.y, r.w, r.h, &mut delay);
                }
                _ => {} // Ignore LongPress, Repeat
            }
        }

        delay.delay_millis(10);
    }
}
