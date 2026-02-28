// Embassy spawned tasks — input polling and housekeeping timers
//
// These tasks replace manual tick-counting and inline polling in the
// main loop with dedicated Embassy tasks that the executor schedules
// via WFI.  The CPU sleeps between events instead of spin-polling.
//
//   • `input_task`        — owns InputDriver + ADC, sends debounced
//                           button events through a Channel and
//                           publishes battery voltage via a Signal.
//
//   • `housekeeping_task` — fires periodic Signals consumed by the
//                           main loop for status-bar refresh, SD
//                           presence checks, and bookmark flushing.
//                           Does NOT touch SD or bookmarks directly
//                           (those are owned by the main loop).

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Ticker, Timer};

use crate::drivers::battery;
use crate::drivers::input::{Event, InputDriver};

// ── Input task ──────────────────────────────────────────────────────────

/// Capacity of the input event channel.
///
/// 8 slots is generous — at 10 ms polling the main loop drains events
/// every tick.  Even during a 1.6 s GC refresh we buffer at most ~3
/// distinct press/release pairs.
pub const INPUT_CHANNEL_CAP: usize = 8;

/// Channel carrying debounced input events from the input task to the
/// main loop.  The main loop calls `try_receive()` (non-blocking) on
/// each tick, or `.receive().await` when idle.
pub static INPUT_EVENTS: Channel<CriticalSectionRawMutex, Event, INPUT_CHANNEL_CAP> =
    Channel::new();

/// Latest battery voltage in millivolts (already converted from raw
/// ADC via `battery::adc_to_battery_mv`).  Updated every ~30 s by the
/// input task.  `Signal` overwrites stale values — the main loop only
/// ever needs the most recent reading.
pub static BATTERY_MV: Signal<CriticalSectionRawMutex, u16> = Signal::new();

/// Ticks (× 10 ms) between battery reads inside the input task.
/// 3000 × 10 ms = 30 s.
const BATTERY_INTERVAL_TICKS: u32 = 3000;

/// The input polling task.
///
/// Owns the [`InputDriver`] (and therefore the ADC hardware).  Runs a
/// 10 ms [`Ticker`], polls the resistor-ladder buttons and power
/// button, and publishes debounced events through [`INPUT_EVENTS`].
/// Every ~30 s it reads the battery ADC and publishes via
/// [`BATTERY_MV`].
///
/// Replacing the inline `input.poll()` calls in the main loop and the
/// `busy_wait_with_input!` macro with channel receives lets Embassy
/// sleep the CPU via WFI between ticks instead of busy-looping.
#[embassy_executor::task]
pub async fn input_task(mut input: InputDriver) -> ! {
    let mut ticker = Ticker::every(Duration::from_millis(10));
    let mut battery_counter: u32 = 0;

    // Take an initial battery reading immediately so the status bar
    // has a value before the first 30 s interval elapses.
    let raw = input.read_battery_mv();
    BATTERY_MV.signal(battery::adc_to_battery_mv(raw));

    loop {
        ticker.next().await;

        // Poll debounced buttons — yields 0 or 1 event per tick.
        if let Some(ev) = input.poll() {
            // try_send: if the channel is full we drop the event.
            // In practice the main loop drains faster than events
            // arrive so this never fires.
            let _ = INPUT_EVENTS.try_send(ev);
        }

        // Periodic battery ADC read
        battery_counter += 1;
        if battery_counter >= BATTERY_INTERVAL_TICKS {
            battery_counter = 0;
            let raw = input.read_battery_mv();
            BATTERY_MV.signal(battery::adc_to_battery_mv(raw));
        }
    }
}

// ── Housekeeping task ───────────────────────────────────────────────────
//
// Fires periodic signals that the main loop checks with
// `signal.try_take()`.  The main loop performs the actual SD / bookmark
// I/O because it owns those resources — this task is purely a timer
// scheduler.

/// Fires every ~5 s — main loop should update the status bar.
pub static STATUS_DUE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Fires every ~30 s — main loop should re-check SD card presence.
pub static SD_CHECK_DUE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Fires every ~30 s — main loop should flush dirty bookmarks.
pub static BOOKMARK_FLUSH_DUE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Background housekeeping timer task.
///
/// Fires periodic [`Signal`]s consumed by the main loop.  Keeps timer
/// state out of the event loop and lets Embassy manage the scheduling
/// efficiently — the CPU sleeps (WFI) between timer expirations.
///
/// The three intervals are:
///   • **Status bar** — every 5 s
///   • **SD card check** — every 30 s
///   • **Bookmark flush** — every 30 s
///
/// SD and bookmark signals are staggered by 2 s so they don't cause a
/// burst of SD I/O on the same tick.
#[embassy_executor::task]
pub async fn housekeeping_task() -> ! {
    // Small initial delay so boot-time rendering finishes before the
    // first housekeeping cycle.
    Timer::after(Duration::from_secs(5)).await;

    let mut status_ticker = Ticker::every(Duration::from_secs(5));
    let mut sd_ticker = Ticker::every(Duration::from_secs(30));

    // Stagger the bookmark ticker by 2 s relative to the SD ticker
    // so they don't both hit SD on the same iteration.
    Timer::after(Duration::from_secs(2)).await;
    let mut bm_ticker = Ticker::every(Duration::from_secs(30));

    loop {
        use embassy_futures::select::{Either3, select3};

        match select3(status_ticker.next(), sd_ticker.next(), bm_ticker.next()).await {
            Either3::First(_) => {
                STATUS_DUE.signal(());
            }
            Either3::Second(_) => {
                SD_CHECK_DUE.signal(());
            }
            Either3::Third(_) => {
                BOOKMARK_FLUSH_DUE.signal(());
            }
        }
    }
}
