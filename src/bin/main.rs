#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::interrupt::Priority;
use esp_hal::time::Duration;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::timer::PeriodicTimer;
use log::info;

use core::cell::RefCell;
use critical_section::Mutex;

use embedded_graphics::mono_font::ascii::FONT_10X20;

use pulp_os::board::Board;
use pulp_os::board::button::Button as HwButton;
use pulp_os::drivers::input::{Event, InputDriver};
use pulp_os::kernel::{AdaptivePoller, Job, Scheduler};
use pulp_os::kernel::wake::{signal_timer, try_wake, WakeReason};
use pulp_os::ui::{Button, Label, Region, Widget};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

// timer interrupt setup
static TIMER0: Mutex<RefCell<Option<PeriodicTimer<'static, esp_hal::Blocking>>>> =
    Mutex::new(RefCell::new(None));

#[esp_hal::handler(priority = Priority::Priority1)]
fn timer0_handler() {
    critical_section::with(|cs| {
        if let Some(timer) = TIMER0.borrow_ref_mut(cs).as_mut() {
            timer.clear_interrupt();
        }
    });
    signal_timer();
}

// test ui 
const TITLE: Region = Region::new(16, 16, 200, 32);
const ITEM0: Region = Region::new(16, 80, 200, 48);
const ITEM1: Region = Region::new(16, 144, 200, 48);
const STATUS: Region = Region::new(16, 220, 300, 32);

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(size: 66320);

    info!("booting...");

    // timer init 10ms tick
    let timg0 = TimerGroup::new(unsafe { peripherals.TIMG0.clone_unchecked() });
    let mut timer0 = PeriodicTimer::new(timg0.timer0);

    critical_section::with(|cs| {
        timer0.set_interrupt_handler(timer0_handler);
        timer0.start(Duration::from_millis(10)).unwrap();
        TIMER0.borrow_ref_mut(cs).replace(timer0);
    });

    info!("timer initialized.");

    // hardware init
    let mut board = Board::init(peripherals);
    let mut delay = Delay::new();

    board.display.epd.init(&mut delay);
    board.display.epd.fill_white();
    info!("hardware initialized.");

    // widgets
    let title = Label::new(TITLE, "pulp-os", &FONT_10X20);
    let mut item0 = Button::new(ITEM0, "Item 0", &FONT_10X20);
    let mut item1 = Button::new(ITEM1, "Item 1", &FONT_10X20);
    let mut status = Label::new(STATUS, "Ready", &FONT_10X20);

    let mut selected: usize = 0;
    item0.set_pressed(true);

    title.draw(&mut board.display.epd).unwrap();
    item0.draw(&mut board.display.epd).unwrap();
    item1.draw(&mut board.display.epd).unwrap();
    status.draw(&mut board.display.epd).unwrap();
    board.display.epd.refresh_full(&mut delay);

    info!("ui ready.");

    let mut scheduler = Scheduler::new();
    let mut poller = AdaptivePoller::new();
    let mut input = InputDriver::new(board.input);

    info!("kernel ready.");

    let mut tick_count: u32 = 0;

    loop {
        while let Some(job) = scheduler.pop() {
            match job {
                Job::RenderPage => {
                    info!("Job: RenderPage");
                }
                Job::PrefetchNext => {
                    info!("Job: PrefetchNext");
                }
                Job::PrefetchPrev => {
                    info!("Job: PrefetchPrev");
                }
                Job::LayoutChapter { chapter } => {
                    info!("Job: LayoutChapter {}", chapter);
                }
                Job::CacheChapter { chapter } => {
                    info!("Job: CacheChapter {}", chapter);
                }
                Job::HandleInput => {}
            }
        }

        let timer_wake = match try_wake() {
            Some(WakeReason::Timer) | Some(WakeReason::Multiple) => true,
            Some(WakeReason::Button) => {
                poller.on_activity();
                true
            }
            Some(WakeReason::Display) => {
                info!("Display ready");
                false
            }
            None => false,
        };

        tick_count += 1;
        let should_poll = timer_wake || (tick_count >= 10);

        if should_poll {
            tick_count = 0;

            if poller.tick() {
                if let Some(event) = input.poll() {
                    poller.on_activity();

                    // Handle input and only act on Press for navigation
                    // LongPress/Repeat only for special actions
                    match event {
                        Event::Press(button) => {
                            info!("[BTN] Press: {}", button.name());

                            match button {
                                HwButton::Right | HwButton::VolUp => {
                                    let old = selected;
                                    selected = (selected + 1) % 2;
                                    if old != selected {
                                        update_selection(
                                            selected,
                                            &mut item0,
                                            &mut item1,
                                            &mut board.display.epd,
                                            &mut delay,
                                        );
                                    }
                                    scheduler.push_or_drop(Job::PrefetchNext);
                                }
                                HwButton::Left | HwButton::VolDown => {
                                    let old = selected;
                                    selected = if selected == 0 { 1 } else { 0 };
                                    if old != selected {
                                        update_selection(
                                            selected,
                                            &mut item0,
                                            &mut item1,
                                            &mut board.display.epd,
                                            &mut delay,
                                        );
                                    }
                                    scheduler.push_or_drop(Job::PrefetchPrev);
                                }
                                HwButton::Confirm => {
                                    let msg = if selected == 0 {
                                        "Selected: Item 0"
                                    } else {
                                        "Selected: Item 1"
                                    };
                                    status.set_text(msg);
                                    status.draw(&mut board.display.epd).unwrap();
                                    let r = status.refresh_bounds();
                                    board.display.epd.refresh_partial(r.x, r.y, r.w, r.h, &mut delay);
                                    scheduler.push_or_drop(Job::RenderPage);
                                }
                                HwButton::Power => {
                                    // TODO: do smth but just log for now
                                    info!("Power pressed");
                                }
                                _ => {}
                            }
                        }
                        Event::Release(button) => {
                            info!("[BTN] Release: {}", button.name());
                        }
                        Event::LongPress(button) => {
                            info!("[BTN] LongPress: {}", button.name());
                            // TODO: could use for special actions like shutdown
                            if button == HwButton::Power {
                                status.set_text("Shutting down...");
                                status.draw(&mut board.display.epd).unwrap();
                                let r = status.refresh_bounds();
                                board.display.epd.refresh_partial(r.x, r.y, r.w, r.h, &mut delay);
                            }
                        }
                        Event::Repeat(button) => {
                            // TODO: figure it out
                            info!("[BTN] Repeat: {}", button.name());
                        }
                    }
                } else {
                    poller.on_idle();
                }
            }
        }

        delay.delay_millis(1);
    }
}

use pulp_os::board::Epd;

fn update_selection(
    selected: usize,
    item0: &mut Button,
    item1: &mut Button,
    display: &mut Epd,
    delay: &mut Delay,
) {
    item0.set_pressed(selected == 0);
    item1.set_pressed(selected == 1);

    item0.draw(display).unwrap();
    item1.draw(display).unwrap();

    // Single refresh for both items
    let r = Region::new(16, 80, 200, 112).align8();
    display.refresh_partial(r.x, r.y, r.w, r.h, delay);

    info!("Selected: Item {}", selected);
}
