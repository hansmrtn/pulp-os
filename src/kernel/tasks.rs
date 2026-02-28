// Embassy spawned tasks — input polling, housekeeping timers, idle sleep
//
// These tasks replace manual tick-counting and inline polling in the
// main loop with dedicated Embassy tasks that the executor schedules
// via WFI.  The CPU sleeps between events instead of spin-polling.
//
//   • `input_task`         — owns InputDriver + ADC, sends debounced
//                            button events through a Channel and
//                            publishes battery voltage via a Signal.
//                            Also resets the idle timer on every event.
//
//   • `housekeeping_task`  — fires periodic Signals consumed by the
//                            main loop for status-bar refresh, SD
//                            presence checks, and bookmark flushing.
//                            Does NOT touch SD or bookmarks directly
//                            (those are owned by the main loop).
//
//   • `idle_timeout_task`  — tracks time since last input activity.
//                            When the configured sleep timeout expires
//                            without any button press, fires
//                            IDLE_SLEEP_DUE so the main loop can put
//                            the display and MCU into deep sleep.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Ticker, Timer};

use crate::drivers::battery;
use crate::drivers::input::{Event, InputDriver};

// ═════════════════════════════════════════════════════════════════════════
// Input task
// ═════════════════════════════════════════════════════════════════════════

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
/// On every published event this task also resets the idle-sleep
/// timer via [`IDLE_RESET`], so the timeout restarts even if the
/// main loop is busy rendering and hasn't drained the channel yet.
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

            // Reset the idle-sleep timer on every user interaction.
            // This happens here (at the source) rather than in the
            // main loop so the timer is bumped even when the main
            // loop is blocked in a long render or SD I/O.
            IDLE_RESET.signal(());
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

// ═════════════════════════════════════════════════════════════════════════
// Housekeeping task
// ═════════════════════════════════════════════════════════════════════════
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

// ═════════════════════════════════════════════════════════════════════════
// Idle-sleep timeout task
// ═════════════════════════════════════════════════════════════════════════
//
// Tracks the elapsed time since the last user interaction and fires
// IDLE_SLEEP_DUE when the configured sleep-timeout expires.  The main
// loop is responsible for the actual sleep sequence (flush bookmarks,
// update status bar, put EPD into deep sleep, enter MCU deep sleep).
//
// Communication with the main loop:
//
//   IDLE_TIMEOUT_MINS  ← main loop sets the configured timeout (0 = never)
//   IDLE_RESET         ← input_task bumps this on every button event
//   IDLE_SLEEP_DUE     → main loop checks this each iteration

/// Signalled by the main loop with the configured sleep timeout in
/// minutes.  Must be signalled at least once at boot (after loading
/// settings) for the idle task to start counting.  Re-signal whenever
/// the setting changes (e.g. user adjusts "Sleep After" in Settings).
///
/// A value of **0** disables idle sleep — the task will park until a
/// non-zero value arrives.
pub static IDLE_TIMEOUT_MINS: Signal<CriticalSectionRawMutex, u16> = Signal::new();

/// Activity reset — signalled by [`input_task`] on every debounced
/// button event.  The idle-timeout task restarts its countdown each
/// time this fires.
///
/// Using a `Signal` (not a Channel) is intentional: multiple rapid
/// button presses collapse into a single "there was activity" flag,
/// which is all the idle timer needs to know.
pub static IDLE_RESET: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Fired when the idle timeout expires without any user interaction.
///
/// The main loop should:
///   1. Flush dirty bookmarks to SD.
///   2. Update the status bar with a "sleeping…" message.
///   3. Put the SSD1677 into deep-sleep mode.
///   4. Enter ESP32-C3 deep sleep with GPIO3 (power button) as wake
///      source.
///
/// On wake the MCU performs a full reset, so there is no "resume"
/// path — boot starts from scratch.
pub static IDLE_SLEEP_DUE: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Convenience: set the idle timeout from the main loop.
///
/// Call once after loading settings, and again whenever the user
/// changes the "Sleep After" value.  Pass `0` to disable.
#[inline]
pub fn set_idle_timeout(minutes: u16) {
    IDLE_TIMEOUT_MINS.signal(minutes);
}

/// Convenience: manually reset the idle timer from the main loop.
///
/// Normally not needed — [`input_task`] already calls this on every
/// button event.  Useful if the main loop considers non-button
/// activity (e.g. completing an SD operation) as "user active".
#[inline]
pub fn reset_idle_timer() {
    IDLE_RESET.signal(());
}

/// Idle-sleep timeout task.
///
/// Waits for the configured timeout to be set via [`IDLE_TIMEOUT_MINS`],
/// then counts down.  Any button event (via [`IDLE_RESET`]) restarts
/// the countdown.  A config change (via [`IDLE_TIMEOUT_MINS`]) also
/// restarts with the new value.  When the countdown reaches zero,
/// [`IDLE_SLEEP_DUE`] is signalled.
///
/// If the timeout is set to 0 (disabled), the task parks efficiently
/// until a non-zero timeout is configured.
#[embassy_executor::task]
pub async fn idle_timeout_task() -> ! {
    // Wait for the first configuration before doing anything.
    let mut timeout_mins = IDLE_TIMEOUT_MINS.wait().await;

    loop {
        // ── Disabled (0 minutes) ────────────────────────────────────
        //
        // Park until the user enables a timeout.  Consumes zero CPU
        // while disabled — Embassy's executor doesn't poll parked tasks.
        if timeout_mins == 0 {
            timeout_mins = IDLE_TIMEOUT_MINS.wait().await;
            continue;
        }

        let duration = Duration::from_secs(timeout_mins as u64 * 60);

        // Drain any stale reset or config signals so we start with a
        // clean slate.  This prevents an old signal from immediately
        // restarting the timer or changing the config.
        let _ = IDLE_RESET.try_take();
        if let Some(new) = IDLE_TIMEOUT_MINS.try_take() {
            timeout_mins = new;
            continue;
        }

        // ── Countdown loop ──────────────────────────────────────────
        //
        // Three possible wake reasons:
        //
        //   1. Activity (IDLE_RESET) — restart the timer.
        //   2. Config change (IDLE_TIMEOUT_MINS) — break to outer loop
        //      to apply the new value.
        //   3. Timer expired — fire IDLE_SLEEP_DUE.
        //
        // IDLE_RESET is checked first in the select so that if both
        // it and the timer resolve simultaneously (astronomically
        // unlikely for a minutes-long timer), activity wins and we
        // don't falsely trigger sleep.

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
                    // User pressed a button — restart the countdown.
                    continue;
                }
                Either3::Second(new_mins) => {
                    // Timeout setting changed (user adjusted in Settings).
                    timeout_mins = new_mins;
                    break; // back to outer loop to re-evaluate
                }
                Either3::Third(()) => {
                    // Countdown expired with no activity.
                    IDLE_SLEEP_DUE.signal(());

                    // Wait for the main loop to act on the signal.
                    // After deep sleep the MCU resets, so in practice
                    // this never returns.  If for some reason sleep is
                    // aborted (e.g. SD flush failed and main loop
                    // decides not to sleep), a button press restarts
                    // everything cleanly.
                    //
                    // Park until the next config or reset signal so we
                    // don't fire IDLE_SLEEP_DUE in a tight loop.
                    use embassy_futures::select::{Either, select};
                    match select(IDLE_RESET.wait(), IDLE_TIMEOUT_MINS.wait()).await {
                        Either::First(()) => {
                            // Activity after aborted sleep — restart timer.
                        }
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
