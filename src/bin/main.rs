#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::time::Duration;
use esp_hal::timer::PeriodicTimer;
use esp_hal::timer::timg::TimerGroup;
use log::info;

use core::cell::RefCell;
use critical_section::Mutex;

use pulp_os::apps::{App, AppId, Launcher, Redraw, Services};
use pulp_os::apps::files::FilesApp;
use pulp_os::apps::home::HomeApp;
use pulp_os::apps::reader::ReaderApp;
use pulp_os::apps::settings::SettingsApp;
use pulp_os::board::Board;
use pulp_os::board::StripBuffer;
use pulp_os::drivers::battery;
use pulp_os::drivers::input::InputDriver;
use pulp_os::drivers::storage::DirCache;
use pulp_os::kernel::wake::{self, signal_timer, try_wake};
use pulp_os::kernel::{Job, Scheduler};
use pulp_os::ui::{StatusBar, SystemStatus, free_stack_bytes, BAR_HEIGHT};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// How often to refresh the status bar (in 10ms ticks). 500 = 5 seconds.
const STATUSBAR_INTERVAL_TICKS: u32 = 500;

// ── Idle timer scaling ─────────────────────────────────────────
//
// The timer interrupt wakes the CPU from WFI to poll ADC buttons.
// During reading, no buttons are pressed for minutes — every 10ms
// wake is wasted power. After a period of inactivity, we slow the
// timer to 100ms (10× fewer wakes). Button presses snap it back.
//
// Power button (GPIO3) has its own interrupt and wakes instantly
// regardless of timer period. ADC buttons get up to ~130ms latency
// on first press during idle (100ms poll + 30ms debounce) — the
// debounce snap-back ensures confirmation is fast once detected.

/// Base timer period (ms). Used during active interaction.
const ACTIVE_TIMER_MS: u64 = 10;
/// Slow timer period (ms). Used after idle threshold.
const IDLE_TIMER_MS: u64 = 100;
/// How many consecutive empty polls before switching to slow timer.
/// 200 × 10ms = 2 seconds of inactivity.
const IDLE_THRESHOLD_POLLS: u32 = 200;

// ── Timer interrupt ────────────────────────────────────────────

static TIMER0: Mutex<RefCell<Option<PeriodicTimer<'static, esp_hal::Blocking>>>> =
    Mutex::new(RefCell::new(None));

#[esp_hal::handler(priority = esp_hal::interrupt::Priority::Priority1)]
fn timer0_handler() {
    critical_section::with(|cs| {
        if let Some(timer) = TIMER0.borrow_ref_mut(cs).as_mut() {
            timer.clear_interrupt();
        }
    });
    signal_timer();
}

/// Change the timer period at runtime. Updates the tick weight so
/// uptime_ticks() stays in consistent 10ms units.
fn set_timer_period(ms: u64) {
    wake::set_tick_weight((ms / ACTIVE_TIMER_MS) as u32);
    critical_section::with(|cs| {
        if let Some(timer) = TIMER0.borrow_ref_mut(cs).as_mut() {
            let _ = timer.start(Duration::from_millis(ms));
        }
    });
}

/// Dispatch to the active app. Apps are stack-allocated — no dyn, no heap.
macro_rules! with_app {
    ($id:expr, $home:expr, $files:expr, $reader:expr, $settings:expr, |$app:ident| $body:expr) => {
        match $id {
            AppId::Home => { let $app = &mut $home; $body }
            AppId::Files => { let $app = &mut $files; $body }
            AppId::Reader => { let $app = &mut $reader; $body }
            AppId::Settings => { let $app = &mut $settings; $body }
        }
    };
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(size: 66320);

    info!("booting...");

    // Timer: 10ms tick
    let timg0 = TimerGroup::new(unsafe { peripherals.TIMG0.clone_unchecked() });
    let mut timer0 = PeriodicTimer::new(timg0.timer0);
    critical_section::with(|cs| {
        timer0.set_interrupt_handler(timer0_handler);
        timer0.start(Duration::from_millis(10)).unwrap();
        timer0.listen();
        TIMER0.borrow_ref_mut(cs).replace(timer0);
    });
    info!("timer initialized.");

    // Hardware
    let mut board = Board::init(peripherals);
    let mut delay = Delay::new();
    board.display.epd.init(&mut delay);
    info!("hardware initialized.");

    let mut strip = StripBuffer::new();

    // Status bar — persistent across all apps
    let mut statusbar = StatusBar::new();
    let sd_ok = board
        .storage
        .sd
        .volume_mgr
        .open_volume(embedded_sdmmc::VolumeIdx(0))
        .is_ok();

    // Apps — all stack-allocated, zero heap
    let mut home = HomeApp::new();
    let mut files = FilesApp::new();
    let mut reader = ReaderApp::new();
    let mut settings = SettingsApp::new();

    // Launcher (owns navigation stack + inter-app context)
    let mut launcher = Launcher::new();

    // Scheduler + input
    let mut sched = Scheduler::new();
    let mut input = InputDriver::new(board.input);
    let mut last_statusbar_ticks: u32 = 0;
    let mut idle_polls: u32 = 0;
    let mut timer_is_slow = false;
    let mut dir_cache = DirCache::new();

    // ── Boot: explicit init, no scheduler ──────────────────────
    home.on_enter(&mut launcher.ctx);
    update_statusbar(&mut statusbar, &mut input, sd_ok);
    board.display.epd.render_full(&mut strip, &mut delay, |s| {
        statusbar.draw(s).unwrap();
        home.draw(s);
    });
    info!("ui ready.");
    info!("kernel ready.");

    // ── Main loop ──────────────────────────────────────────────
    loop {
        // 1. Drain all pending jobs by priority
        while let Some(job) = sched.pop() {
            match job {
                // ── PollInput (High) ───────────────────────────
                Job::PollInput => {
                    let Some(event) = input.poll() else {
                        // No confirmed event yet.
                        //
                        // If debouncing (raw activity, not yet confirmed),
                        // snap timer to fast so confirmation arrives in
                        // ~10ms instead of ~100ms. Worst-case first-press
                        // latency: 100ms (idle poll) + 30ms (debounce) = 130ms
                        // instead of 200ms without this check.
                        if timer_is_slow && input.is_debouncing() {
                            set_timer_period(ACTIVE_TIMER_MS);
                            timer_is_slow = false;
                            idle_polls = 0;
                        }

                        idle_polls += 1;
                        if !timer_is_slow && idle_polls >= IDLE_THRESHOLD_POLLS {
                            set_timer_period(IDLE_TIMER_MS);
                            timer_is_slow = true;
                            info!("timer: {}ms (idle)", IDLE_TIMER_MS);
                        }
                        continue;
                    };

                    // Got input — snap back to fast timer
                    if timer_is_slow {
                        set_timer_period(ACTIVE_TIMER_MS);
                        timer_is_slow = false;
                        info!("timer: {}ms (active)", ACTIVE_TIMER_MS);
                    }
                    idle_polls = 0;

                    // Route to active app
                    let active = launcher.active();
                    let transition =
                        with_app!(active, home, files, reader, settings, |app| {
                            app.on_event(event, &mut launcher.ctx)
                        });

                    // Apply navigation
                    if let Some(nav) = launcher.apply(transition) {
                        info!("app: {:?} -> {:?}", nav.from, nav.to);

                        // Departing app: suspend if staying on stack, exit if leaving
                        if nav.suspend {
                            with_app!(nav.from, home, files, reader, settings, |app| {
                                app.on_suspend();
                            });
                        } else {
                            with_app!(nav.from, home, files, reader, settings, |app| {
                                app.on_exit();
                            });
                        }

                        // Arriving app: resume if returning, enter if fresh
                        if nav.resume {
                            with_app!(nav.to, home, files, reader, settings, |app| {
                                app.on_resume(&mut launcher.ctx);
                            });
                        } else {
                            with_app!(nav.to, home, files, reader, settings, |app| {
                                app.on_enter(&mut launcher.ctx);
                            });
                        }
                    } else {
                        // No navigation — dirty regions (if any) were
                        // already pushed into ctx by on_event via mark_dirty().
                    }

                    // ── Cascade: enqueue follow-on work ────────
                    //
                    // RENDER OWNERSHIP INVARIANT:
                    // When an app has pending async work, IT owns the
                    // render decision. PollInput must not enqueue Render
                    // alongside AppWork — doing so renders stale data
                    // before the work completes, then renders again
                    // after (double refresh, one wasted).
                    //
                    // The `else if` enforces this structurally.
                    let active = launcher.active();
                    let needs = with_app!(active, home, files, reader, settings, |app| {
                        app.needs_work()
                    });
                    if needs {
                        let _ = sched.push_unique(Job::AppWork);
                    } else if launcher.ctx.has_redraw() {
                        let _ = sched.push_unique(Job::Render);
                    }
                }

                // ── Render (High) ──────────────────────────────
                Job::Render => {
                    let active = launcher.active();
                    match launcher.ctx.take_redraw() {
                        Redraw::Full => {
                            update_statusbar(&mut statusbar, &mut input, sd_ok);
                            with_app!(active, home, files, reader, settings, |app| {
                                board.display.epd.render_full(
                                    &mut strip,
                                    &mut delay,
                                    |s| {
                                        statusbar.draw(s).unwrap();
                                        app.draw(s);
                                    },
                                );
                            });
                        }
                        Redraw::Partial(r) => {
                            let r = r.align8();
                            let bar_overlaps = r.y < BAR_HEIGHT;
                            with_app!(active, home, files, reader, settings, |app| {
                                board.display.epd.render_partial(
                                    &mut strip,
                                    r.x,
                                    r.y,
                                    r.w,
                                    r.h,
                                    &mut delay,
                                    |s| {
                                        if bar_overlaps {
                                            statusbar.draw(s).unwrap();
                                        }
                                        app.draw(s);
                                    },
                                );
                            });
                        }
                        Redraw::None => {} // Race: already consumed
                    }
                }

                // ── AppWork (Normal) ────────────────────────────
                //
                // Generic async work for the active app. The kernel
                // constructs Services (just two refs, zero cost) and
                // calls on_work(). The app handles everything — cache
                // management, error handling, dirty region marking.
                //
                // No app-specific code lives here. Adding a new app
                // with async I/O needs zero changes to this handler.
                Job::AppWork => {
                    let active = launcher.active();
                    let mut svc = Services::new(&mut dir_cache, &board.storage.sd);
                    with_app!(active, home, files, reader, settings, |app| {
                        app.on_work(&mut svc, &mut launcher.ctx);
                    });
                    if launcher.ctx.has_redraw() {
                        let _ = sched.push_unique(Job::Render);
                    }
                }

                // ── UpdateStatusBar (Normal) ───────────────────
                Job::UpdateStatusBar => {
                    update_statusbar(&mut statusbar, &mut input, sd_ok);
                    // Don't enqueue a render — the next full refresh
                    // will pick up the new text. Avoids unnecessary
                    // partial refreshes just for the status bar.
                }
            }
        }

        // 2. Wait for wake events
        let wake = match try_wake() {
            Some(w) => w,
            None => {
                wake::wait_for_interrupt();
                continue;
            }
        };

        // 3. Translate wake flags into jobs
        //
        // Each flag is checked independently — concurrent sources
        // (e.g. Timer + Display) all get handled, nothing swallowed.
        //
        // Button and Timer both poll input: button because the user
        // pressed something, timer because ADC-based buttons are
        // sampled on the tick.
        if wake.has_input() {
            // Power button GPIO interrupt — snap to fast timer immediately.
            // ADC buttons snap back via is_debouncing() in PollInput,
            // giving ~130ms worst-case latency (100ms + 30ms debounce).
            if wake.button && timer_is_slow {
                set_timer_period(ACTIVE_TIMER_MS);
                timer_is_slow = false;
                idle_polls = 0;
                info!("timer: {}ms (button wake)", ACTIVE_TIMER_MS);
            }

            let _ = sched.push_unique(Job::PollInput);

            let ticks = wake::uptime_ticks();
            if ticks.wrapping_sub(last_statusbar_ticks) >= STATUSBAR_INTERVAL_TICKS {
                last_statusbar_ticks = ticks;
                let _ = sched.push_unique(Job::UpdateStatusBar);
            }
        }

        // Display BUSY interrupt completion — currently no-op.
        // Will be used when BUSY pin drives a GPIO interrupt
        // to signal end-of-refresh without busy-waiting.
        if wake.display {
            // future: signal display-done to unblock render
        }
    }
}

fn update_statusbar(bar: &mut StatusBar, input: &mut InputDriver, sd_ok: bool) {
    const HEAP_TOTAL: usize = 66320;
    let stats = esp_alloc::HEAP.stats();

    let adc_mv = input.read_battery_mv();
    let bat_mv = battery::adc_to_battery_mv(adc_mv);
    let bat_pct = battery::battery_percentage(bat_mv);

    bar.update(&SystemStatus {
        uptime_secs: wake::uptime_secs(),
        battery_mv: bat_mv,
        battery_pct: bat_pct,
        heap_used: stats.current_usage,
        heap_total: HEAP_TOTAL,
        stack_free: free_stack_bytes(),
        sd_ok,
    });
}
