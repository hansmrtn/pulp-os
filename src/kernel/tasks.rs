// embassy spawned tasks: input polling, housekeeping, idle sleep

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Ticker, Timer};

use crate::drivers::battery;
use crate::drivers::input::{Event, InputDriver};

pub const INPUT_CHANNEL_CAP: usize = 8;
pub static INPUT_EVENTS: Channel<CriticalSectionRawMutex, Event, INPUT_CHANNEL_CAP> =
    Channel::new();

pub static BATTERY_MV: Signal<CriticalSectionRawMutex, u16> = Signal::new();

const BATTERY_INTERVAL_TICKS: u32 = 3000; // 3000 x 10 ms = 30 s

#[embassy_executor::task]
pub async fn input_task(mut input: InputDriver) -> ! {
    let mut ticker = Ticker::every(Duration::from_millis(10));
    let mut battery_counter: u32 = 0;

    let raw = input.read_battery_mv();
    BATTERY_MV.signal(battery::adc_to_battery_mv(raw));

    loop {
        ticker.next().await;

        if let Some(ev) = input.poll() {
            let _ = INPUT_EVENTS.try_send(ev);
            IDLE_RESET.signal(());
        }

        battery_counter += 1;
        if battery_counter >= BATTERY_INTERVAL_TICKS {
            battery_counter = 0;
            let raw = input.read_battery_mv();
            BATTERY_MV.signal(battery::adc_to_battery_mv(raw));
        }
    }
}

pub static STATUS_DUE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static SD_CHECK_DUE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static BOOKMARK_FLUSH_DUE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

#[embassy_executor::task]
pub async fn housekeeping_task() -> ! {
    Timer::after(Duration::from_secs(5)).await;

    let mut status_ticker = Ticker::every(Duration::from_secs(5));
    let mut sd_ticker = Ticker::every(Duration::from_secs(30));

    Timer::after(Duration::from_secs(2)).await; // stagger behind SD
    let mut bm_ticker = Ticker::every(Duration::from_secs(30));

    loop {
        use embassy_futures::select::{Either3, select3};

        match select3(status_ticker.next(), sd_ticker.next(), bm_ticker.next()).await {
            Either3::First(_) => STATUS_DUE.signal(()),
            Either3::Second(_) => SD_CHECK_DUE.signal(()),
            Either3::Third(_) => BOOKMARK_FLUSH_DUE.signal(()),
        }
    }
}

pub static IDLE_TIMEOUT_MINS: Signal<CriticalSectionRawMutex, u16> = Signal::new();
pub static IDLE_RESET: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static IDLE_SLEEP_DUE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

#[inline]
pub fn set_idle_timeout(minutes: u16) {
    IDLE_TIMEOUT_MINS.signal(minutes);
}

#[embassy_executor::task]
pub async fn idle_timeout_task() -> ! {
    let mut timeout_mins = IDLE_TIMEOUT_MINS.wait().await;

    loop {
        if timeout_mins == 0 {
            timeout_mins = IDLE_TIMEOUT_MINS.wait().await;
            continue;
        }

        let duration = Duration::from_secs(timeout_mins as u64 * 60);

        let _ = IDLE_RESET.try_take();
        if let Some(new) = IDLE_TIMEOUT_MINS.try_take() {
            timeout_mins = new;
            continue;
        }

        loop {
            use embassy_futures::select::{Either3, select3};

            match select3(
                IDLE_RESET.wait(),
                IDLE_TIMEOUT_MINS.wait(),
                Timer::after(duration),
            )
            .await
            {
                Either3::First(()) => {
                    continue;
                }
                Either3::Second(new_mins) => {
                    timeout_mins = new_mins;
                    break;
                }
                Either3::Third(()) => {
                    IDLE_SLEEP_DUE.signal(());

                    use embassy_futures::select::{Either, select};
                    match select(IDLE_RESET.wait(), IDLE_TIMEOUT_MINS.wait()).await {
                        Either::First(()) => {}
                        Either::Second(new_mins) => {
                            timeout_mins = new_mins;
                            break;
                        }
                    }
                }
            }
        }
    }
}
