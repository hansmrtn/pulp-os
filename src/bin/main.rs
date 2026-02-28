// pulp-os entry point — Embassy async main loop
//
// Replaces the hand-rolled cooperative scheduler + block_on with
// Embassy's executor.  A single async task drives everything:
//
//   • 10 ms Ticker for input polling (replaces TIMER0 ISR + wake flags)
//   • `select(busy_pin, ticker)` during display refresh lets us process
//     input and pre-load the next page while the e-paper waveform runs
//   • Direct `.await` on display BUSY pin — CPU sleeps (WFI) instead
//     of spin-polling
//
// The App trait, all format/UI/driver code, and the strip-buffer
// rendering pipeline are completely unchanged.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::timer::timg::TimerGroup;
use log::info;

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Ticker};

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
use pulp_os::drivers::storage::{self, DirCache};
use pulp_os::drivers::strip::StripBuffer;
use pulp_os::kernel::uptime_secs;
use pulp_os::ui::quick_menu::{MAX_APP_ACTIONS, QuickMenuResult};
use pulp_os::ui::{
    BAR_HEIGHT, ButtonFeedback, QuickMenu, StatusBar, SystemStatus, free_stack_bytes,
};
use static_cell::StaticCell;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

// ── Constants ───────────────────────────────────────────────────────────

const TICK_MS: u64 = 10;

/// Number of ticks between status-bar updates (500 × 10 ms = 5 s).
const STATUS_INTERVAL: u32 = 500;

const DEFAULT_GHOST_CLEAR_EVERY: u32 = 10;
const SD_CHECK_EVERY: u32 = 6; // × STATUS_INTERVAL ≈ 30 s
const BATTERY_READ_EVERY: u32 = 6; // × STATUS_INTERVAL ≈ 30 s

// ── App dispatch macro (unchanged) ──────────────────────────────────────

macro_rules! with_app {
    ($id:expr, $home:expr, $files:expr, $reader:expr, $settings:expr,
     |$app:ident| $body:expr) => {
        match $id {
            AppId::Home => {
                let $app = &mut *$home;
                $body
            }
            AppId::Files => {
                let $app = &mut *$files;
                $body
            }
            AppId::Reader => {
                let $app = &mut *$reader;
                $body
            }
            AppId::Settings => {
                let $app = &mut *$settings;
                $body
            }
        }
    };
}

// ── Transition handler (shared between normal events and deferred) ──────

macro_rules! apply_transition {
    ($nav:expr, $launcher:expr, $home:expr, $files:expr,
     $reader:expr, $settings:expr, $bm_cache:expr) => {{
        let nav = $nav;
        info!("app: {:?} -> {:?}", nav.from, nav.to);

        if nav.from == AppId::Reader {
            $reader.save_position($bm_cache);
        }

        if nav.suspend {
            with_app!(nav.from, $home, $files, $reader, $settings, |app| {
                app.on_suspend();
            });
        } else {
            with_app!(nav.from, $home, $files, $reader, $settings, |app| {
                app.on_exit();
            });
        }

        // Propagate persisted prefs before lifecycle callbacks
        {
            let ui_idx = $settings.system_settings().ui_font_size_idx;
            let book_idx = $settings.system_settings().book_font_size_idx;
            if nav.to == AppId::Reader {
                $reader.set_book_font_size(book_idx);
            }
            $home.set_ui_font_size(ui_idx);
            $files.set_ui_font_size(ui_idx);
            $settings.set_ui_font_size(ui_idx);
        }

        if nav.resume {
            with_app!(nav.to, $home, $files, $reader, $settings, |app| {
                app.on_resume(&mut $launcher.ctx);
            });
        } else {
            with_app!(nav.to, $home, $files, $reader, $settings, |app| {
                app.on_enter(&mut $launcher.ctx);
            });
        }
    }};
}

// ── BUSY-wait loop with input processing ────────────────────────────────
//
// Used for both full and partial refreshes.  While the display
// controller is running a waveform (~400 ms DU, ~1.6 s GC), we
// `select` between the BUSY pin going low and the next ticker tick.
// On each tick we poll input and — critically — dispatch page-turn
// events so that `on_work` can pre-load the next page from SD.
// Transitions (Back, Home) are deferred until the waveform completes.

macro_rules! busy_wait_with_input {
    ($epd:expr, $ticker:expr, $input:expr, $mapper:expr,
     $quick_menu:expr, $launcher:expr,
     $home:expr, $files:expr, $reader:expr, $settings:expr,
     $dir_cache:expr, $bm_cache:expr, $sd:expr) => {{
        let mut _deferred: Option<Transition> = None;
        loop {
            // Level-check first; avoids creating a future if already done.
            if !$epd.is_busy() {
                break;
            }
            let result = select($epd.busy_pin().wait_for_low(), $ticker.next()).await;
            match result {
                Either::First(_) => break,
                Either::Second(_) => {
                    let Some(hw_event) = $input.poll() else {
                        continue;
                    };
                    let event = $mapper.map_event(hw_event);

                    // Skip quick-menu interactions during refresh;
                    // they're cosmetic and can wait.
                    if $quick_menu.open {
                        continue;
                    }

                    let active = $launcher.active();
                    let t = with_app!(active, $home, $files, $reader, $settings, |app| app
                        .on_event(event, &mut $launcher.ctx));
                    if !matches!(t, Transition::None) && _deferred.is_none() {
                        _deferred = Some(t);
                    }

                    // Pre-load the next page while the waveform runs.
                    let active = $launcher.active();
                    let needs = with_app!(active, $home, $files, $reader, $settings, |app| app
                        .needs_work());
                    if needs {
                        let mut svc = Services::new($dir_cache, $bm_cache, &$sd);
                        with_app!(active, $home, $files, $reader, $settings, |app| app
                            .on_work(&mut svc, &mut $launcher.ctx));
                    }
                }
            }
        }
        _deferred
    }};
}

// ═════════════════════════════════════════════════════════════════════════
// Heavy statics — kept OUT of the async future so the state machine
// that Embassy stores is ~200 bytes instead of 50 KB+.  Each local
// becomes a thin `&'static mut` (4 bytes on riscv32).
//
// Types whose `new()` is `const fn` use `ConstStaticCell`: the value
// is placed directly in `.bss` at link time with ZERO stack cost.
// This is critical for `ReaderApp` (~25 KB) which would overflow the
// ESP32-C3's 8 KB stack if constructed as a temporary.
//
// Types whose `new()` calls runtime functions (font lookups) use the
// normal `StaticCell` — they're small enough (< 1 KB) for the stack.
// ═════════════════════════════════════════════════════════════════════════

/// Const-initialized static cell.  The value lives in `.bss`/`.data`
/// at link time — **zero stack usage**.  `as_static_mut()` hands out
/// a `&'static mut` reference; call it exactly once (single-core init).
struct ConstStaticCell<T>(core::cell::UnsafeCell<T>);
unsafe impl<T> Sync for ConstStaticCell<T> {}
impl<T> ConstStaticCell<T> {
    const fn new(val: T) -> Self {
        Self(core::cell::UnsafeCell::new(val))
    }
    #[allow(clippy::mut_from_ref)]
    fn as_static_mut(&self) -> &'static mut T {
        unsafe { &mut *self.0.get() }
    }
}

// const-fn types → ConstStaticCell (zero stack cost)
static STRIP: ConstStaticCell<StripBuffer> = ConstStaticCell::new(StripBuffer::new());
static STATUSBAR: ConstStaticCell<StatusBar> = ConstStaticCell::new(StatusBar::new());
static READER: ConstStaticCell<ReaderApp> = ConstStaticCell::new(ReaderApp::new());
static LAUNCHER: ConstStaticCell<Launcher> = ConstStaticCell::new(Launcher::new());
static QUICK_MENU: ConstStaticCell<QuickMenu> = ConstStaticCell::new(QuickMenu::new());
static BUMPS: ConstStaticCell<ButtonFeedback> = ConstStaticCell::new(ButtonFeedback::new());
static DIR_CACHE: ConstStaticCell<DirCache> = ConstStaticCell::new(DirCache::new());
static BM_CACHE: ConstStaticCell<BookmarkCache> = ConstStaticCell::new(BookmarkCache::new());

// non-const types (call runtime font functions) → StaticCell (< 1 KB each, fine on stack)
static HOME: StaticCell<HomeApp> = StaticCell::new();
static FILES: StaticCell<FilesApp> = StaticCell::new();
static SETTINGS: StaticCell<SettingsApp> = StaticCell::new();

// ═════════════════════════════════════════════════════════════════════════
// Entry point
// ═════════════════════════════════════════════════════════════════════════

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    // 230 KB heap.  The strip-buffer architecture saves 44 KB vs a full
    // framebuffer, so we trade 16 KB of that headroom back to the stack
    // where esp-rtos's deeper call chain (executor → task → app) needs it.
    esp_alloc::heap_allocator!(size: 235520);

    info!("booting...");

    // ── esp-rtos scheduler + Embassy time driver ────────────────────────
    //
    // `esp_rtos::start` initialises the RTOS scheduler and, critically,
    // the hardware-backed time driver that Embassy's `Timer`, `Ticker`
    // and `Instant` depend on.  On RISC-V targets it also needs a
    // software-interrupt channel for context switches.
    //
    // This MUST run before the first `.await` — the `#[esp_rtos::main]`
    // macro sets up the Embassy executor but does NOT call `start()`.
    let timg0 = TimerGroup::new(unsafe { peripherals.TIMG0.clone_unchecked() });
    let sw_ints =
        SoftwareInterruptControl::new(unsafe { peripherals.SW_INTERRUPT.clone_unchecked() });
    esp_rtos::start(timg0.timer0, sw_ints.software_interrupt0);
    info!("esp-rtos scheduler started (TIMG0 + SW_INT0).");

    // ── Hardware ────────────────────────────────────────────────────────

    let mut board = Board::init(peripherals);
    let mut delay = Delay::new();
    board.display.epd.init(&mut delay);
    info!("hardware initialized.");

    let strip = STRIP.as_static_mut();

    let statusbar = STATUSBAR.as_static_mut();
    let mut sd_ok = board
        .storage
        .sd
        .volume_mgr
        .open_volume(embedded_sdmmc::VolumeIdx(0))
        .is_ok();

    // ensure _PULP app-data directory exists
    if sd_ok {
        if let Err(e) = storage::ensure_pulp_dir(&board.storage.sd) {
            info!("warning: failed to create _PULP dir: {}", e);
        }
    }

    // ── Input ───────────────────────────────────────────────────────────

    let mut input = InputDriver::new(board.input);
    let mapper = ButtonMapper::new();

    // ── Applications ────────────────────────────────────────────────────

    let home = HOME.init(HomeApp::new());
    let files = FILES.init(FilesApp::new());
    let reader = READER.as_static_mut();
    let settings = SETTINGS.init(SettingsApp::new());

    let launcher = LAUNCHER.as_static_mut();
    let quick_menu = QUICK_MENU.as_static_mut();
    let bumps = BUMPS.as_static_mut();

    let dir_cache = DIR_CACHE.as_static_mut();
    let bm_cache = BM_CACHE.as_static_mut();

    bm_cache.ensure_loaded(&board.storage.sd);

    // Load settings + recent book before first render
    {
        let mut svc = Services::new(dir_cache, bm_cache, &board.storage.sd);
        settings.load_eager(&mut svc);
        let ui_idx = settings.system_settings().ui_font_size_idx;
        let book_idx = settings.system_settings().book_font_size_idx;
        home.set_ui_font_size(ui_idx);
        files.set_ui_font_size(ui_idx);
        settings.set_ui_font_size(ui_idx);
        reader.set_book_font_size(book_idx);
        home.load_recent(&mut svc);
    }

    // ── Initial render ──────────────────────────────────────────────────

    let cached_battery_mv_init = battery::adc_to_battery_mv(input.read_battery_mv());
    update_statusbar(statusbar, cached_battery_mv_init, sd_ok);

    home.on_enter(&mut launcher.ctx);

    // Write strips (polls input between strips), then kick GC.
    board.display.epd.write_full_frame_progressive(
        strip,
        &mut delay,
        &|s: &mut StripBuffer| {
            statusbar.draw(s).unwrap();
            home.draw(s);
        },
        || {
            let _ = input.poll();
        },
    );
    board.display.epd.start_full_update();

    // CPU sleeps (WFI) during the ~1.6 s GC waveform.
    board.display.epd.busy_pin().wait_for_low().await;
    board.display.epd.finish_full_update();

    // Drain stale redraw left by on_enter
    let _ = launcher.ctx.take_redraw();
    info!("ui ready.");
    info!("kernel ready.");

    // ═════════════════════════════════════════════════════════════════════
    // Main event loop
    // ═════════════════════════════════════════════════════════════════════

    let mut ticker = Ticker::every(Duration::from_millis(TICK_MS));

    let mut partial_refreshes: u32 = 0;
    let mut status_counter: u32 = 0;
    let mut sd_check_counter: u32 = 0;
    let mut battery_read_counter: u32 = 0;
    let mut cached_battery_mv: u16 = cached_battery_mv_init;
    #[allow(unused_assignments)]
    let mut render_bar_overlaps: bool = false;

    loop {
        // ── 1. Wait for next tick ───────────────────────────────────────
        //
        // The CPU enters WFI here.  Any interrupt (timer, GPIO) wakes it.
        // If we fell behind (e.g. slow SD I/O), the ticker fires
        // immediately for each missed tick until we catch up.
        ticker.next().await;

        // ── 2. Poll and process input ───────────────────────────────────

        if let Some(hw_event) = input.poll() {
            // Button feedback (edge labels)
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

            // ── Quick-menu intercept ────────────────────────────────────

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
                                quick_menu,
                                launcher.active(),
                                home,
                                files,
                                reader,
                                settings,
                                &mut launcher.ctx,
                            );
                            launcher.ctx.mark_dirty(region);
                        }
                        QuickMenuResult::RefreshScreen => {
                            sync_quick_menu(
                                quick_menu,
                                launcher.active(),
                                home,
                                files,
                                reader,
                                settings,
                                &mut launcher.ctx,
                            );
                            launcher.ctx.request_full_redraw();
                        }
                        QuickMenuResult::GoHome => {
                            sync_quick_menu(
                                quick_menu,
                                launcher.active(),
                                home,
                                files,
                                reader,
                                settings,
                                &mut launcher.ctx,
                            );
                            let transition = Transition::Home;
                            if let Some(nav) = launcher.apply(transition) {
                                apply_transition!(
                                    nav, launcher, home, files, reader, settings, bm_cache
                                );
                            }
                        }
                        QuickMenuResult::AppTrigger(id) => {
                            let active = launcher.active();
                            let region = quick_menu.region();
                            sync_quick_menu(
                                quick_menu,
                                active,
                                home,
                                files,
                                reader,
                                settings,
                                &mut launcher.ctx,
                            );
                            with_app!(active, home, files, reader, settings, |app| {
                                app.on_quick_trigger(id, &mut launcher.ctx);
                            });
                            if active == AppId::Reader {
                                reader.save_position(bm_cache);
                            }
                            launcher.ctx.mark_dirty(region);
                        }
                    }
                }
            }
            // ── Menu toggle ─────────────────────────────────────────────
            else if matches!(event, ActionEvent::Press(Action::Menu)) {
                let active = launcher.active();
                let actions: &[_] = with_app!(active, home, files, reader, settings, |app| {
                    app.quick_actions()
                });
                quick_menu.show(actions);
                launcher.ctx.mark_dirty(quick_menu.region());
            }
            // ── Normal app dispatch ─────────────────────────────────────
            else {
                let active = launcher.active();
                let transition = with_app!(active, home, files, reader, settings, |app| {
                    app.on_event(event, &mut launcher.ctx)
                });

                if let Some(nav) = launcher.apply(transition) {
                    apply_transition!(nav, launcher, home, files, reader, settings, bm_cache);
                }
            }
        }

        // ── 3. App work ─────────────────────────────────────────────────
        //
        // Run one work step per tick.  Multi-step operations (EPUB init,
        // chapter caching) return after each SD I/O so input can be
        // polled between steps.
        {
            let active = launcher.active();
            let needs = with_app!(active, home, files, reader, settings, |app| {
                app.needs_work()
            });
            if needs {
                let mut svc = Services::new(dir_cache, bm_cache, &board.storage.sd);
                with_app!(active, home, files, reader, settings, |app| {
                    app.on_work(&mut svc, &mut launcher.ctx);
                });
            }
        }

        // ── 4. Periodic housekeeping ────────────────────────────────────

        status_counter += 1;
        if status_counter >= STATUS_INTERVAL {
            status_counter = 0;

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

            update_statusbar(statusbar, cached_battery_mv, sd_ok);
        }

        // ── 5. Render ───────────────────────────────────────────────────

        if !launcher.ctx.has_redraw() {
            continue;
        }

        let redraw = launcher.ctx.take_redraw();

        // Try partial refresh; fall through to full on ghost-clear
        // promotion, initial-refresh requirement, or explicit Full.
        'render: {
            if let Redraw::Partial(r) = redraw {
                let ghost_clear_every = if settings.is_loaded() {
                    settings.system_settings().ghost_clear_every as u32
                } else {
                    DEFAULT_GHOST_CLEAR_EVERY
                };

                if partial_refreshes < ghost_clear_every {
                    let r = r.align8();
                    render_bar_overlaps = r.y < BAR_HEIGHT;

                    // Phase 1: write new content to BW RAM
                    let active = launcher.active();
                    let rs = with_app!(active, home, files, reader, settings, |app| {
                        board.display.epd.partial_phase1_bw(
                            strip,
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
                        // Phase 2: kick DU waveform (~400-600 ms)
                        board.display.epd.partial_start_du(&rs);

                        // Process input while DU runs — the key
                        // snappiness win.  Page turns dispatched here
                        // trigger on_work to pre-load the next page
                        // from SD, so by the time DU finishes the new
                        // content is ready to render immediately.
                        let deferred = busy_wait_with_input!(
                            board.display.epd,
                            ticker,
                            input,
                            mapper,
                            quick_menu,
                            launcher,
                            home,
                            files,
                            reader,
                            settings,
                            dir_cache,
                            bm_cache,
                            board.storage.sd
                        );

                        // Phase 3: sync both RAM planes
                        let active = launcher.active();
                        with_app!(active, home, files, reader, settings, |app| {
                            board.display.epd.partial_phase3_sync(
                                strip,
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

                        // Phase 4: power off (async — CPU sleeps ~200 ms)
                        board.display.epd.power_off_async().await;
                        partial_refreshes += 1;

                        // Apply any transition that was deferred during
                        // the BUSY wait (Back, Home, etc.)
                        if let Some(transition) = deferred {
                            if let Some(nav) = launcher.apply(transition) {
                                apply_transition!(
                                    nav, launcher, home, files, reader, settings, bm_cache
                                );
                            }
                        }

                        break 'render;
                    }

                    // partial_phase1_bw returned None
                    if !board.display.epd.needs_initial_refresh() {
                        // Degenerate zero-size region; skip.
                        break 'render;
                    }
                    // Fall through to full GC (initial refresh needed)
                    info!("display: partial failed (initial refresh), promoting to full");
                } else {
                    info!("display: promoted partial to full (ghosting clear)");
                }
                // Fall through to full GC (ghost clear)
            }

            // ── Full GC refresh ─────────────────────────────────────────
            //
            // Reached by: explicit Redraw::Full, ghost-clear promotion,
            // or initial-refresh fallback.

            if matches!(redraw, Redraw::Full | Redraw::Partial(_)) {
                update_statusbar(statusbar, cached_battery_mv, sd_ok);

                let active = launcher.active();
                with_app!(active, home, files, reader, settings, |app| {
                    board.display.epd.write_full_frame_progressive(
                        strip,
                        &mut delay,
                        &|s: &mut StripBuffer| {
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
                    );
                });

                board.display.epd.start_full_update();

                // Process input during the ~1.6 s GC waveform.
                let deferred = busy_wait_with_input!(
                    board.display.epd,
                    ticker,
                    input,
                    mapper,
                    quick_menu,
                    launcher,
                    home,
                    files,
                    reader,
                    settings,
                    dir_cache,
                    bm_cache,
                    board.storage.sd
                );

                board.display.epd.finish_full_update();
                partial_refreshes = 0;

                if let Some(transition) = deferred {
                    if let Some(nav) = launcher.apply(transition) {
                        apply_transition!(nav, launcher, home, files, reader, settings, bm_cache);
                    }
                }
            }
        } // 'render
    } // main loop
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn update_statusbar(bar: &mut StatusBar, battery_mv: u16, sd_ok: bool) {
    const HEAP_TOTAL: usize = 235520;
    let stats = esp_alloc::HEAP.stats();

    let bat_pct = battery::battery_percentage(battery_mv);

    bar.update(&SystemStatus {
        uptime_secs: uptime_secs(),
        battery_mv,
        battery_pct: bat_pct,
        heap_used: stats.current_usage,
        heap_total: HEAP_TOTAL,
        stack_free: free_stack_bytes(),
        sd_ok,
    });
}

/// Push quick-menu cycle changes into the active app and persist
/// settings-owned values (e.g. font size) to the settings struct.
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

    // If the reader's font-size cycle changed, persist into settings
    if active == AppId::Reader {
        if let Some(font_idx) = qm.app_cycle_value(1) {
            let ss = settings.system_settings_mut();
            if ss.book_font_size_idx != font_idx {
                ss.book_font_size_idx = font_idx;
                settings.mark_save_needed();
            }
        }
    }
}
