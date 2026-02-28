// pulp-os entry point and main loop
//
// Boot: timer → hardware → UI → Home. Main loop: drain scheduler → WFI → repeat.
// Apps stack-allocated, dispatched via with_app! (no dyn). Timer scales 10ms→100ms
// on idle. Display refresh async via block_on; CPU sleeps (WFI) during e-paper wait.

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

use pulp_os::apps::files::FilesApp;
use pulp_os::apps::home::HomeApp;
use pulp_os::apps::reader::ReaderApp;
use pulp_os::apps::settings::SettingsApp;
use pulp_os::apps::{
    App, AppContext, AppId, BookmarkCache, Launcher, Redraw, Services, Transition,
};
use pulp_os::board::Board;
use pulp_os::board::action::{Action, ActionEvent, ButtonMapper};
use pulp_os::drivers::battery;
use pulp_os::drivers::input::InputDriver;
use pulp_os::drivers::ssd1677::RenderState;
use pulp_os::drivers::storage::{self, DirCache};
use pulp_os::drivers::strip::StripBuffer;
use pulp_os::kernel::wake::{self, try_wake};
use pulp_os::kernel::{Job, Scheduler, block_on};
use pulp_os::ui::quick_menu::{MAX_APP_ACTIONS, QuickMenuResult};
use pulp_os::ui::{
    BAR_HEIGHT, ButtonFeedback, QuickMenu, StatusBar, SystemStatus, free_stack_bytes,
};

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const STATUSBAR_INTERVAL_TICKS: u32 = 500; // 5 seconds in 10ms ticks

const ACTIVE_TIMER_MS: u64 = 10;
const IDLE_TIMER_MS: u64 = 100;
const IDLE_THRESHOLD_POLLS: u32 = 50; // 50 * 10ms = 500ms before idle

const DEFAULT_GHOST_CLEAR_EVERY: u32 = 10; // fallback before settings load
const SD_CHECK_EVERY: u32 = 6;             // ~30s at 5s status interval
const BATTERY_READ_EVERY: u32 = 6;         // ~30s; avoids ADC on every 5s tick

static TIMER0: Mutex<RefCell<Option<PeriodicTimer<'static, esp_hal::Blocking>>>> =
    Mutex::new(RefCell::new(None));

#[esp_hal::handler(priority = esp_hal::interrupt::Priority::Priority1)]
fn timer0_handler() {
    critical_section::with(|cs| {
        if let Some(timer) = TIMER0.borrow_ref_mut(cs).as_mut() {
            timer.clear_interrupt();
        }
    });
    wake::signal_timer();
}

fn set_timer_period(ms: u64) {
    wake::set_tick_weight((ms / ACTIVE_TIMER_MS) as u32);
    critical_section::with(|cs| {
        if let Some(timer) = TIMER0.borrow_ref_mut(cs).as_mut() {
            let _ = timer.start(Duration::from_millis(ms));
        }
    });
}

macro_rules! with_app {
    ($id:expr, $home:expr, $files:expr, $reader:expr, $settings:expr, |$app:ident| $body:expr) => {
        match $id {
            AppId::Home => {
                let $app = &mut $home;
                $body
            }
            AppId::Files => {
                let $app = &mut $files;
                $body
            }
            AppId::Reader => {
                let $app = &mut $reader;
                $body
            }
            AppId::Settings => {
                let $app = &mut $settings;
                $body
            }
        }
    };
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(size: 246480);

    info!("booting...");

    let timg0 = TimerGroup::new(unsafe { peripherals.TIMG0.clone_unchecked() });
    let mut timer0 = PeriodicTimer::new(timg0.timer0);
    critical_section::with(|cs| {
        timer0.set_interrupt_handler(timer0_handler);
        timer0.start(Duration::from_millis(10)).unwrap();
        timer0.listen();
        TIMER0.borrow_ref_mut(cs).replace(timer0);
    });
    info!("timer initialized.");

    let mut board = Board::init(peripherals);
    let mut delay = Delay::new();
    board.display.epd.init(&mut delay);
    info!("hardware initialized.");

    let mut strip = StripBuffer::new();

    let mut statusbar = StatusBar::new();
    let mut sd_ok = board
        .storage
        .sd
        .volume_mgr
        .open_volume(embedded_sdmmc::VolumeIdx(0))
        .is_ok();

    // ensure _PULP app-data directory exists before any config I/O
    if sd_ok && let Err(e) = storage::ensure_pulp_dir(&board.storage.sd) {
        info!("warning: failed to create _PULP dir: {}", e);
    }

    let mut home = HomeApp::new();
    let mut files = FilesApp::new();
    let mut reader = ReaderApp::new();
    let mut settings = SettingsApp::new();

    let mut launcher = Launcher::new();
    let mapper = ButtonMapper::new();
    let mut quick_menu = QuickMenu::new();
    let mut bumps = ButtonFeedback::new();

    let mut sched = Scheduler::new();
    let mut input = InputDriver::new(board.input);
    let mut last_statusbar_ticks: u32 = 0;
    let mut idle_polls: u32 = 0;
    let mut timer_is_slow = false;
    let mut partial_refreshes: u32 = 0;
    let mut dir_cache = DirCache::new();
    let mut bm_cache = BookmarkCache::new();
    let mut sd_check_counter: u32 = 0;
    let mut battery_read_counter: u32 = 0;
    let mut cached_battery_mv: u16 = battery::adc_to_battery_mv(input.read_battery_mv());
    let mut render_state: Option<RenderState> = None;
    let mut render_bar_overlaps: bool = false;

    bm_cache.ensure_loaded(&board.storage.sd);

    // load settings + recent book before first render
    {
        let mut svc = Services::new(&mut dir_cache, &mut bm_cache, &board.storage.sd);
        settings.load_eager(&mut svc);
        let ui_idx = settings.system_settings().ui_font_size_idx;
        let book_idx = settings.system_settings().book_font_size_idx;
        home.set_ui_font_size(ui_idx);
        files.set_ui_font_size(ui_idx);
        settings.set_ui_font_size(ui_idx);
        reader.set_book_font_size(book_idx);
        home.load_recent(&mut svc);
    }

    home.on_enter(&mut launcher.ctx);
    update_statusbar(&mut statusbar, cached_battery_mv, sd_ok);
    block_on(board.display.epd.render_full_async_progressive(
        &mut strip,
        &mut delay,
        |s| {
            statusbar.draw(s).unwrap();
            home.draw(s);
        },
        || {
            let _ = input.poll();
        },
    ));
    // drain stale redraw left by on_enter before entering the loop
    let _ = launcher.ctx.take_redraw();
    info!("ui ready.");
    info!("kernel ready.");

    loop {
        // drain all pending jobs by priority (high first, FIFO within tier)
        while let Some(job) = sched.pop() {
            match job {
                Job::PollInput => {
                    let Some(hw_event) = input.poll() else {
                        if timer_is_slow && input.is_debouncing() {
                            set_timer_period(ACTIVE_TIMER_MS);
                            timer_is_slow = false;
                            idle_polls = 0;
                            continue;
                        }

                        idle_polls += 1;
                        if !timer_is_slow && idle_polls >= IDLE_THRESHOLD_POLLS {
                            set_timer_period(IDLE_TIMER_MS);
                            timer_is_slow = true;
                            info!("timer: {}ms (idle)", IDLE_TIMER_MS);
                        }
                        continue;
                    };

                    if timer_is_slow {
                        set_timer_period(ACTIVE_TIMER_MS);
                        timer_is_slow = false;
                        info!("timer: {}ms (active)", ACTIVE_TIMER_MS);
                    }
                    idle_polls = 0;

                    match hw_event {
                        pulp_os::drivers::input::Event::Press(btn) => {
                            if let Some(r) = bumps.on_press(btn) {
                                launcher.ctx.mark_dirty(r);
                            }
                        }
                        pulp_os::drivers::input::Event::Release(_) => {
                            if let Some(r) = bumps.on_release() {
                                launcher.ctx.mark_dirty(r);
                            }
                        }
                        _ => {}
                    }

                    let event = mapper.map_event(hw_event);

                    // ── Quick menu intercept ────────────────────────
                    if quick_menu.open {
                        if let ActionEvent::Press(action) | ActionEvent::Repeat(action) = event {
                            let result = quick_menu.on_action(action);
                            match result {
                                QuickMenuResult::Consumed => {
                                    if quick_menu.dirty {
                                        launcher.ctx.mark_dirty(quick_menu.region());
                                        quick_menu.dirty = false;
                                    }
                                }
                                QuickMenuResult::Close => {
                                    let region = quick_menu.region();
                                    sync_quick_menu(
                                        &quick_menu,
                                        launcher.active(),
                                        &mut home,
                                        &mut files,
                                        &mut reader,
                                        &mut settings,
                                        &mut launcher.ctx,
                                    );
                                    launcher.ctx.mark_dirty(region);
                                }
                                QuickMenuResult::RefreshScreen => {
                                    sync_quick_menu(
                                        &quick_menu,
                                        launcher.active(),
                                        &mut home,
                                        &mut files,
                                        &mut reader,
                                        &mut settings,
                                        &mut launcher.ctx,
                                    );
                                    launcher.ctx.request_full_redraw();
                                }
                                QuickMenuResult::GoHome => {
                                    sync_quick_menu(
                                        &quick_menu,
                                        launcher.active(),
                                        &mut home,
                                        &mut files,
                                        &mut reader,
                                        &mut settings,
                                        &mut launcher.ctx,
                                    );
                                        let transition = Transition::Home;
                                        if let Some(nav) = launcher.apply(transition) {
                                            info!(
                                                "app: {:?} -> {:?} (quick menu GoHome)",
                                                nav.from, nav.to
                                            );
                                            if nav.from == AppId::Reader {
                                            reader.save_position(&mut bm_cache);
                                        }
                                        with_app!(nav.from, home, files, reader, settings, |app| {
                                            app.on_exit();
                                        });

                                        {
                                            let ui_idx =
                                                settings.system_settings().ui_font_size_idx;
                                            let book_idx =
                                                settings.system_settings().book_font_size_idx;
                                            if nav.to == AppId::Reader {
                                                reader.set_book_font_size(book_idx);
                                            }
                                            home.set_ui_font_size(ui_idx);
                                            files.set_ui_font_size(ui_idx);
                                            settings.set_ui_font_size(ui_idx);
                                        }

                                        with_app!(nav.to, home, files, reader, settings, |app| {
                                            app.on_resume(&mut launcher.ctx);
                                        });
                                    }
                                }
                                QuickMenuResult::AppTrigger(id) => {
                                    let active = launcher.active();
                                    let region = quick_menu.region();
                                    sync_quick_menu(
                                        &quick_menu,
                                        active,
                                        &mut home,
                                        &mut files,
                                        &mut reader,
                                        &mut settings,
                                        &mut launcher.ctx,
                                    );
                                    with_app!(active, home, files, reader, settings, |app| {
                                        app.on_quick_trigger(id, &mut launcher.ctx);
                                    });
                                    // reader triggers always flush position to SD
                                    if active == AppId::Reader {
                                        reader.save_position(&mut bm_cache);
                                    }
                                    launcher.ctx.mark_dirty(region);
                                    let needs =
                                        with_app!(active, home, files, reader, settings, |app| {
                                            app.needs_work()
                                        });
                                    if needs {
                                        let _ = sched.push_unique(Job::AppWork);
                                    }
                                }
                            }
                        }

                    if launcher.ctx.has_redraw() {
                            let _ = sched.push_unique(Job::Render);
                        }
                        continue;
                    }

                    // ── Menu toggle (open overlay) ──────────────────
                    if matches!(event, ActionEvent::Press(Action::Menu)) {
                        let active = launcher.active();
                        let actions: &[_] =
                            with_app!(active, home, files, reader, settings, |app| {
                                app.quick_actions()
                            });
                        quick_menu.show(actions);
                        launcher.ctx.mark_dirty(quick_menu.region());
                        let _ = sched.push_unique(Job::Render);
                        continue;
                    }

                    // ── Normal app dispatch ─────────────────────────
                    let active = launcher.active();
                    let transition = with_app!(active, home, files, reader, settings, |app| {
                        app.on_event(event, &mut launcher.ctx)
                    });

                    if let Some(nav) = launcher.apply(transition) {
                        info!("app: {:?} -> {:?}", nav.from, nav.to);

                    if nav.from == AppId::Reader {
                        reader.save_position(&mut bm_cache);
                    }

                    if nav.suspend {
                            with_app!(nav.from, home, files, reader, settings, |app| {
                                app.on_suspend();
                            });
                        } else {
                            with_app!(nav.from, home, files, reader, settings, |app| {
                                app.on_exit();
                            });
                        }

                        // propagate persisted prefs before lifecycle callbacks
                        {
                            let ui_idx = settings.system_settings().ui_font_size_idx;
                            let book_idx = settings.system_settings().book_font_size_idx;
                            if nav.to == AppId::Reader {
                                reader.set_book_font_size(book_idx);
                            }

                            home.set_ui_font_size(ui_idx);
                            files.set_ui_font_size(ui_idx);
                            settings.set_ui_font_size(ui_idx);
                        }

                        if nav.resume {
                            with_app!(nav.to, home, files, reader, settings, |app| {
                                app.on_resume(&mut launcher.ctx);
                            });
                        } else {
                            with_app!(nav.to, home, files, reader, settings, |app| {
                                app.on_enter(&mut launcher.ctx);
                            });
                        }
                    }

                    // let AppWork own render decision to avoid double refresh
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

                Job::Render => {
                    // don't start a new render while split-phase partial is in progress
                    if render_state.is_some() {
                        continue;
                    }
                    let active = launcher.active();
                    match launcher.ctx.take_redraw() {
                        Redraw::Full => {
                            update_statusbar(&mut statusbar, cached_battery_mv, sd_ok);
                            with_app!(active, home, files, reader, settings, |app| {
                                block_on(board.display.epd.render_full_async_progressive(
                                    &mut strip,
                                    &mut delay,
                                    |s| {
                                        statusbar.draw(s).unwrap();
                                        app.draw(s);
                                        if quick_menu.open {
                                            quick_menu.draw(s);
                                        }
                                        bumps.draw(s);
                                    },
                                    || {
                                        let _ = input.poll();
                                    },
                                ));
                            });
                            partial_refreshes = 0;
                        }
                        Redraw::Partial(r) => {
                            let ghost_clear_every = if settings.is_loaded() {
                                settings.system_settings().ghost_clear_every as u32
                            } else {
                                DEFAULT_GHOST_CLEAR_EVERY
                            };
                            if partial_refreshes >= ghost_clear_every {
                                // promote to full GC to clear ghosting
                                update_statusbar(&mut statusbar, cached_battery_mv, sd_ok);
                                with_app!(active, home, files, reader, settings, |app| {
                                    block_on(board.display.epd.render_full_async_progressive(
                                        &mut strip,
                                        &mut delay,
                                        |s| {
                                            statusbar.draw(s).unwrap();
                                            app.draw(s);
                                            if quick_menu.open {
                                                quick_menu.draw(s);
                                            }
                                            bumps.draw(s);
                                        },
                                        || {
                                            let _ = input.poll();
                                        },
                                    ));
                                });
                                partial_refreshes = 0;
                                info!("display: promoted partial to full (ghosting clear)");
                            } else {
                                // split-phase partial: write BW, kick DU, yield to scheduler
                                let r = r.align8();
                                render_bar_overlaps = r.y < BAR_HEIGHT;
                                idle_polls = 0; // keep active timer during refresh

                                let rs = with_app!(active, home, files, reader, settings, |app| {
                                    board.display.epd.partial_phase1_bw(
                                        &mut strip,
                                        r.x,
                                        r.y,
                                        r.w,
                                        r.h,
                                        &mut delay,
                                        &|s: &mut StripBuffer| {
                                            if render_bar_overlaps {
                                                statusbar.draw(s).unwrap();
                                            }
                                            app.draw(s);
                                            if quick_menu.open {
                                                quick_menu.draw(s);
                                            }
                                            bumps.draw(s);
                                        },
                                    )
                                });

                                if let Some(rs) = rs {
                                    board.display.epd.partial_start_du(&rs);
                                    render_state = Some(rs);
                                    partial_refreshes += 1;
                                    let _ = sched.push_unique(Job::RenderPhase2);
                                } else if board.display.epd.needs_initial_refresh() {
                                    update_statusbar(&mut statusbar, cached_battery_mv, sd_ok);
                                    with_app!(active, home, files, reader, settings, |app| {
                                        block_on(board.display.epd.render_full_async_progressive(
                                            &mut strip,
                                            &mut delay,
                                            |s| {
                                                statusbar.draw(s).unwrap();
                                                app.draw(s);
                                                if quick_menu.open {
                                                    quick_menu.draw(s);
                                                }
                                                bumps.draw(s);
                                            },
                                            || {
                                                let _ = input.poll();
                                            },
                                        ));
                                    });
                                    partial_refreshes = 0;
                                }
                                // else: degenerate region (zero-size), skip
                            }
                        }
                        Redraw::None => {}
                    }
                }

                Job::RenderPhase2 => {
                    // Normal priority; PollInput (High) runs first on each wake
                    if board.display.epd.is_busy() {
                        let _ = sched.push_unique(Job::RenderPhase2);
                        break; // WFI until next interrupt instead of spin-polling
                    }
                    let _ = sched.push_unique(Job::RenderPhase3);
                }

                Job::RenderPhase3 => {
                    if let Some(rs) = render_state.take() {
                        let active = launcher.active();
                        with_app!(active, home, files, reader, settings, |app| {
                            board.display.epd.partial_phase3_sync(
                                &mut strip,
                                &rs,
                                &|s: &mut StripBuffer| {
                                    if render_bar_overlaps {
                                        statusbar.draw(s).unwrap();
                                    }
                                    app.draw(s);
                                    if quick_menu.open {
                                        quick_menu.draw(s);
                                    }
                                    bumps.draw(s);
                                },
                            );
                        });
                        board.display.epd.power_off(); // ~200ms, WFI inside wait_busy
                    }
                    if launcher.ctx.has_redraw() {
                        let _ = sched.push_unique(Job::Render);
                    }
                }

                Job::AppWork => {
                    let active = launcher.active();
                    let mut svc = Services::new(&mut dir_cache, &mut bm_cache, &board.storage.sd);
                    with_app!(active, home, files, reader, settings, |app| {
                        app.on_work(&mut svc, &mut launcher.ctx);
                    });
                    if launcher.ctx.has_redraw() {
                        let _ = sched.push_unique(Job::Render);
                    }
                }

                Job::UpdateStatusBar => {
                    sd_check_counter += 1;
                    if sd_check_counter >= SD_CHECK_EVERY {
                        sd_check_counter = 0;
                        sd_ok = board
                            .storage
                            .sd
                            .volume_mgr
                            .open_volume(embedded_sdmmc::VolumeIdx(0))
                            .is_ok();
                    }
                    battery_read_counter += 1;
                    if battery_read_counter >= BATTERY_READ_EVERY {
                        battery_read_counter = 0;
                        cached_battery_mv = battery::adc_to_battery_mv(input.read_battery_mv());
                    }
                    if bm_cache.is_dirty() {
                        bm_cache.flush(&board.storage.sd);
                    }
                    update_statusbar(&mut statusbar, cached_battery_mv, sd_ok);
                }
            }
        }

        let wake = match try_wake() {
            Some(w) => w,
            None => {
                wake::wait_for_interrupt();
                continue;
            }
        };

        if wake.has_input() {
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

    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn update_statusbar(bar: &mut StatusBar, battery_mv: u16, sd_ok: bool) {
    const HEAP_TOTAL: usize = 256720;
    let stats = esp_alloc::HEAP.stats();

    let bat_pct = battery::battery_percentage(battery_mv);

    bar.update(&SystemStatus {
        uptime_secs: wake::uptime_secs(),
        battery_mv,
        battery_pct: bat_pct,
        heap_used: stats.current_usage,
        heap_total: HEAP_TOTAL,
        stack_free: free_stack_bytes(),
        sd_ok,
    });
}

// sync quick menu cycle changes to active app; persist settings-owned values
fn sync_quick_menu(
    qm: &QuickMenu,
    active: AppId,
    home: &mut HomeApp,
    files: &mut FilesApp,
    reader: &mut ReaderApp,
    settings: &mut SettingsApp,
    ctx: &mut AppContext,
) {
    for id in 0..MAX_APP_ACTIONS as u8 {
        if let Some(value) = qm.app_cycle_value(id) {
            match active {
                AppId::Home => home.on_quick_cycle_update(id, value, ctx),
                AppId::Files => files.on_quick_cycle_update(id, value, ctx),
                AppId::Reader => reader.on_quick_cycle_update(id, value, ctx),
                AppId::Settings => settings.on_quick_cycle_update(id, value, ctx),
            }
        }
    }

    if active == AppId::Reader
        && let Some(font_idx) = qm.app_cycle_value(1)
    {
        let ss = settings.system_settings_mut();
        if ss.book_font_size_idx != font_idx {
            ss.book_font_size_idx = font_idx;
            settings.mark_save_needed();
        }
    }
}
