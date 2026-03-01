// pulp-os entry point: Embassy multi-task architecture.
// main:              UI event loop, app dispatch, rendering.
// input_task:        10ms button poll, publishes events + battery mv.
// housekeeping_task: periodic signals (status 5s, SD/bookmarks 30s).
// idle_timeout_task: fires IDLE_SLEEP_DUE after idle timeout.
// CPU sleeps (WFI) whenever all tasks are waiting.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::RtcPinWithResistors;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::rtc_cntl::Rtc;
use esp_hal::rtc_cntl::sleep::{RtcioWakeupSource, WakeupLevel};
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
use pulp_os::fonts;
use pulp_os::kernel::tasks;
use pulp_os::kernel::uptime_secs;
use pulp_os::ui::quick_menu::{MAX_APP_ACTIONS, QuickMenuResult};
use pulp_os::ui::{
    BAR_HEIGHT, ButtonFeedback, QuickMenu, StatusBar, SystemStatus, free_stack_bytes, paint_stack,
    stack_high_water_mark,
};
use static_cell::StaticCell;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

// on_work cadence: lets multi-step ops (EPUB init, caching) progress between events
const TICK_MS: u64 = 10;

const DEFAULT_GHOST_CLEAR_EVERY: u32 = 6;

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
            AppId::Upload => {
                unreachable!("Upload mode is handled outside the app dispatch loop");
            }
        }
    };
}

macro_rules! apply_transition {
    ($nav:expr, $launcher:expr, $home:expr, $files:expr,
     $reader:expr, $settings:expr, $bm_cache:expr,
     $quick_menu:expr, $bumps:expr) => {{
        let nav = $nav;
        info!("app: {:?} -> {:?}", nav.from, nav.to);

        if nav.from == AppId::Reader {
            $reader.save_position($bm_cache);
        }

        if nav.from != AppId::Upload {
            if nav.suspend {
                with_app!(nav.from, $home, $files, $reader, $settings, |app| {
                    app.on_suspend();
                });
            } else {
                with_app!(nav.from, $home, $files, $reader, $settings, |app| {
                    app.on_exit();
                });
            }
        }

        // propagate persisted prefs before lifecycle callbacks
        {
            let ui_idx = $settings.system_settings().ui_font_size_idx;
            let book_idx = $settings.system_settings().book_font_size_idx;
            if nav.to == AppId::Reader {
                $reader.set_book_font_size(book_idx);
            }
            $home.set_ui_font_size(ui_idx);
            $files.set_ui_font_size(ui_idx);
            $settings.set_ui_font_size(ui_idx);
            let chrome = fonts::chrome_font(ui_idx);
            $quick_menu.set_chrome_font(chrome);
            $bumps.set_chrome_font(chrome);
            $reader.set_chrome_font(chrome);
        }

        if nav.to != AppId::Upload {
            if nav.resume {
                with_app!(nav.to, $home, $files, $reader, $settings, |app| {
                    app.on_resume(&mut $launcher.ctx);
                });
            } else {
                with_app!(nav.to, $home, $files, $reader, $settings, |app| {
                    app.on_enter(&mut $launcher.ctx);
                });
            }
        }
    }};
}

// busy-wait loop with input processing.
// runs during full and partial waveforms; selects on BUSY pin, input channel,
// and work ticker so page pre-loads happen concurrently.
// non-trivial transitions (Back, Home) deferred until waveform ends.
macro_rules! busy_wait_with_input {
    ($epd:expr, $mapper:expr,
     $quick_menu:expr, $launcher:expr,
     $home:expr, $files:expr, $reader:expr, $settings:expr,
     $dir_cache:expr, $bm_cache:expr, $sd:expr) => {{
        let mut _deferred: Option<Transition> = None;
        let mut _work_ticker = Ticker::every(Duration::from_millis(TICK_MS));
        loop {
            // level-check first; avoids creating futures if already done
            if !$epd.is_busy() {
                break;
            }

            // wait for BUSY low, input event, or work tick
            match select(
                $epd.busy_pin().wait_for_low(),
                select(tasks::INPUT_EVENTS.receive(), _work_ticker.next()),
            )
            .await
            {
                Either::First(_) => break,

                // input event from the channel
                Either::Second(Either::First(hw_event)) => {
                    let event = $mapper.map_event(hw_event);

                    // skip quick-menu during refresh; cosmetic, can wait
                    if $quick_menu.open {
                        continue;
                    }

                    let active = $launcher.active();
                    let t = with_app!(active, $home, $files, $reader, $settings, |app| app
                        .on_event(event, &mut $launcher.ctx));
                    if !matches!(t, Transition::None) && _deferred.is_none() {
                        _deferred = Some(t);
                    }
                }

                // work tick
                Either::Second(Either::Second(_)) => {}
            }

            // pre-load next page while waveform runs
            let active = $launcher.active();
            let needs = with_app!(active, $home, $files, $reader, $settings, |app| app
                .needs_work());
            if needs {
                let mut svc = Services::new($dir_cache, $bm_cache, &$sd);
                with_app!(active, $home, $files, $reader, $settings, |app| app
                    .on_work(&mut svc, &mut $launcher.ctx));
            }
        }
        _deferred
    }};
}

// heavy statics kept out of async future so Embassy's state machine stays ~200B.
// const-fn types -> ConstStaticCell (zero stack, placed in .bss).
// runtime-init types -> StaticCell.

// value in .bss at link time; as_static_mut() called once
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

static STRIP: ConstStaticCell<StripBuffer> = ConstStaticCell::new(StripBuffer::new());
static STATUSBAR: ConstStaticCell<StatusBar> = ConstStaticCell::new(StatusBar::new());
static READER: ConstStaticCell<ReaderApp> = ConstStaticCell::new(ReaderApp::new());
static LAUNCHER: ConstStaticCell<Launcher> = ConstStaticCell::new(Launcher::new());
static QUICK_MENU: ConstStaticCell<QuickMenu> = ConstStaticCell::new(QuickMenu::new());
static BUMPS: ConstStaticCell<ButtonFeedback> = ConstStaticCell::new(ButtonFeedback::new());
static DIR_CACHE: ConstStaticCell<DirCache> = ConstStaticCell::new(DirCache::new());
static BM_CACHE: ConstStaticCell<BookmarkCache> = ConstStaticCell::new(BookmarkCache::new());

static HOME: StaticCell<HomeApp> = StaticCell::new();
static FILES: StaticCell<FilesApp> = StaticCell::new();
static SETTINGS: StaticCell<SettingsApp> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // paint sentinel before any deep calls; measure peak via stack_high_water_mark()
    paint_stack();

    // 140KB heap (reduced from 200KB to fit WiFi firmware blobs in DRAM).
    // WiFi radio static data ~65KB; this leaves enough for stack + esp-radio.
    esp_alloc::heap_allocator!(size: 143360);

    info!("booting...");

    // must run before first .await; sets up RTOS scheduler + Embassy timer driver
    let timg0 = TimerGroup::new(unsafe { peripherals.TIMG0.clone_unchecked() });
    let sw_ints =
        SoftwareInterruptControl::new(unsafe { peripherals.SW_INTERRUPT.clone_unchecked() });
    esp_rtos::start(timg0.timer0, sw_ints.software_interrupt0);
    info!("esp-rtos scheduler started (TIMG0 + SW_INT0).");

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

    // ensure _PULP/ exists
    if sd_ok && let Err(e) = storage::ensure_pulp_dir(&board.storage.sd) {
        info!("warning: failed to create _PULP dir: {}", e);
    }

    // created here for initial battery read, then moved into input_task
    let mut input = InputDriver::new(board.input);
    let mapper = ButtonMapper::new();

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

    // load settings + recent book before first render
    {
        let mut svc = Services::new(dir_cache, bm_cache, &board.storage.sd);
        settings.load_eager(&mut svc);
        let ui_idx = settings.system_settings().ui_font_size_idx;
        let book_idx = settings.system_settings().book_font_size_idx;
        home.set_ui_font_size(ui_idx);
        files.set_ui_font_size(ui_idx);
        settings.set_ui_font_size(ui_idx);
        reader.set_book_font_size(book_idx);
        let chrome = fonts::chrome_font(ui_idx);
        quick_menu.set_chrome_font(chrome);
        bumps.set_chrome_font(chrome);
        reader.set_chrome_font(chrome);
        home.load_recent(&mut svc);
    }

    // signal idle timeout after settings load so persisted value is used
    tasks::set_idle_timeout(settings.system_settings().sleep_timeout);

    let cached_battery_mv_init = battery::adc_to_battery_mv(input.read_battery_mv());
    update_statusbar(statusbar, cached_battery_mv_init, sd_ok);

    home.on_enter(&mut launcher.ctx);

    // write both RAM planes, kick GC waveform, yield ~1.6s
    board
        .display
        .epd
        .full_refresh_async(strip, &mut delay, &|s: &mut StripBuffer| {
            statusbar.draw(s).unwrap();
            home.draw(s);
        })
        .await;

    // drain stale redraw left by on_enter
    let _ = launcher.ctx.take_redraw();
    info!("ui ready.");

    // InputDriver moved into input_task; events arrive via INPUT_EVENTS from here on
    spawner.spawn(tasks::input_task(input)).unwrap();
    spawner.spawn(tasks::housekeeping_task()).unwrap();
    spawner.spawn(tasks::idle_timeout_task()).unwrap();
    info!("tasks spawned (input_task, housekeeping_task, idle_timeout_task).");
    info!("kernel ready.");

    // main event loop: wakes on input event or work ticker (10ms)
    let mut work_ticker = Ticker::every(Duration::from_millis(TICK_MS));

    let mut partial_refreshes: u32 = 0;
    let mut cached_battery_mv: u16 = cached_battery_mv_init;
    let mut red_stale: bool = false;
    #[allow(unused_assignments)]
    let mut render_bar_overlaps: bool = false;

    loop {
        // 0. upload mode intercept: bypasses App trait, runs own async loop
        if launcher.active() == AppId::Upload {
            let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };
            pulp_os::apps::upload::run_upload_mode(
                wifi,
                &mut board.display.epd,
                strip,
                &mut delay,
                &board.storage.sd,
                settings.system_settings().ui_font_size_idx,
                bumps,
            )
            .await;

            // pop back and re-render
            if let Some(nav) = launcher.apply(Transition::Pop) {
                apply_transition!(
                    nav, launcher, home, files, reader, settings, bm_cache, quick_menu, bumps
                );
            }
            launcher.ctx.request_full_redraw();
            continue;
        }

        // 1. wait for input or work tick
        let hw_event = match select(tasks::INPUT_EVENTS.receive(), work_ticker.next()).await {
            Either::First(ev) => Some(ev),
            Either::Second(_) => None,
        };

        // 2. input event
        if let Some(hw_event) = hw_event {
            // button feedback (edge labels)
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

            // power long-press: intercept before mapping so no app sees it
            if hw_event
                == pulp_os::drivers::input::Event::LongPress(pulp_os::board::button::Button::Power)
            {
                info!("power held: entering sleep...");

                // flush dirty bookmarks
                if bm_cache.is_dirty() {
                    bm_cache.flush(&board.storage.sd);
                }

                // render sleep screen (~1.6s GC waveform)
                board
                    .display
                    .epd
                    .full_refresh_async(strip, &mut delay, &|s: &mut StripBuffer| {
                        use embedded_graphics::mono_font::MonoTextStyle;
                        use embedded_graphics::mono_font::ascii::FONT_6X13;
                        use embedded_graphics::pixelcolor::BinaryColor;
                        use embedded_graphics::prelude::*;
                        use embedded_graphics::text::Text;

                        let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::On);
                        let _ = Text::new("(sleep)", Point::new(210, 400), style).draw(s);
                    })
                    .await;
                info!("display: sleep screen rendered");

                // SSD1677 deep-sleep mode 1 (~3uA, image retained; hw reset to wake)
                board.display.epd.enter_deep_sleep();
                info!("display: deep sleep mode 1");

                // ESP32-C3 deep sleep (~5uA); GPIO3 RTC wake; MCU resets on wake
                let mut rtc = Rtc::new(unsafe { esp_hal::peripherals::LPWR::steal() });
                let mut gpio3 = unsafe { esp_hal::peripherals::GPIO3::steal() };
                let wakeup_pins: &mut [(&mut dyn RtcPinWithResistors, WakeupLevel)] =
                    &mut [(&mut gpio3, WakeupLevel::Low)];
                let rtcio = RtcioWakeupSource::new(wakeup_pins);

                info!("mcu: entering deep sleep (power button to wake)");
                rtc.sleep_deep(&[&rtcio]);
            }

            let event = mapper.map_event(hw_event);

            // quick-menu
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
                                    nav, launcher, home, files, reader, settings, bm_cache,
                                    quick_menu, bumps
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
            // menu toggle
            else if matches!(event, ActionEvent::Press(Action::Menu)) {
                let active = launcher.active();
                let actions: &[_] = with_app!(active, home, files, reader, settings, |app| {
                    app.quick_actions()
                });
                quick_menu.show(actions);
                launcher.ctx.mark_dirty(quick_menu.region());
            }
            // app dispatch
            else {
                let active = launcher.active();
                let transition = with_app!(active, home, files, reader, settings, |app| {
                    app.on_event(event, &mut launcher.ctx)
                });

                if let Some(nav) = launcher.apply(transition) {
                    apply_transition!(
                        nav, launcher, home, files, reader, settings, bm_cache, quick_menu, bumps
                    );
                }
            }
        }

        // if we just landed on Upload, skip to top where intercept lives
        if launcher.active() == AppId::Upload {
            continue;
        }

        // 3. app work: one step per iteration; multi-step ops yield between SD reads
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

        // 4. housekeeping

        // battery mv (~30s, from input_task)
        if let Some(mv) = tasks::BATTERY_MV.try_take() {
            cached_battery_mv = mv;
        }

        // SD presence check (~30s)
        if tasks::SD_CHECK_DUE.try_take().is_some() {
            sd_ok = board
                .storage
                .sd
                .volume_mgr
                .open_volume(embedded_sdmmc::VolumeIdx(0))
                .is_ok();
        }

        // bookmark flush (~30s)
        if tasks::BOOKMARK_FLUSH_DUE.try_take().is_some() && bm_cache.is_dirty() {
            bm_cache.flush(&board.storage.sd);
        }

        // status bar refresh (~5s)
        if tasks::STATUS_DUE.try_take().is_some() {
            update_statusbar(statusbar, cached_battery_mv, sd_ok);

            // re-sync idle timeout in case settings changed
            if settings.is_loaded() {
                tasks::set_idle_timeout(settings.system_settings().sleep_timeout);
            }
        }

        // idle sleep: flush, sleep screen, deep sleep; wake = full reset
        if tasks::IDLE_SLEEP_DUE.try_take().is_some() {
            info!("idle timeout: entering sleep...");

            // flush dirty bookmarks
            if bm_cache.is_dirty() {
                bm_cache.flush(&board.storage.sd);
            }

            // render sleep screen (~1.6s GC waveform)
            board
                .display
                .epd
                .full_refresh_async(strip, &mut delay, &|s: &mut StripBuffer| {
                    use embedded_graphics::mono_font::MonoTextStyle;
                    use embedded_graphics::mono_font::ascii::FONT_6X13;
                    use embedded_graphics::pixelcolor::BinaryColor;
                    use embedded_graphics::prelude::*;
                    use embedded_graphics::text::Text;

                    let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::On);
                    let _ = Text::new("(sleep)", Point::new(210, 400), style).draw(s);
                })
                .await;
            info!("display: sleep screen rendered");

            // SSD1677 deep-sleep mode 1 (~3uA, image retained; hw reset to wake)
            board.display.epd.enter_deep_sleep();
            info!("display: deep sleep mode 1");

            // ESP32-C3 deep sleep (~5uA); GPIO3 RTC wake; MCU resets on wake
            // steal peripherals: ownership irrelevant, sleep_deep() is -> !
            let mut rtc = Rtc::new(unsafe { esp_hal::peripherals::LPWR::steal() });
            let mut gpio3 = unsafe { esp_hal::peripherals::GPIO3::steal() };
            let wakeup_pins: &mut [(&mut dyn RtcPinWithResistors, WakeupLevel)] =
                &mut [(&mut gpio3, WakeupLevel::Low)];
            let rtcio = RtcioWakeupSource::new(wakeup_pins);

            info!("mcu: entering deep sleep (power button to wake)");
            rtc.sleep_deep(&[&rtcio]);
        }

        // 5. render
        if !launcher.ctx.has_redraw() {
            continue;
        }

        let redraw = launcher.ctx.take_redraw();

        // try partial; fall through to full on ghost-clear, initial refresh, or explicit Full
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

                    // phase 1: write BW; if red_stale also write RED=!BW so DU drives all pixels
                    let active = launcher.active();
                    let rs = with_app!(active, home, files, reader, settings, |app| {
                        let draw = |s: &mut StripBuffer| {
                            if render_bar_overlaps {
                                statusbar.draw(s).unwrap();
                            }
                            app.draw(s);
                            if quick_menu.open {
                                quick_menu.draw(s);
                            }
                            bumps.draw(s);
                        };
                        if red_stale {
                            board.display.epd.partial_phase1_bw_inv_red(
                                strip, r.x, r.y, r.w, r.h, &mut delay, &draw,
                            )
                        } else {
                            board
                                .display
                                .epd
                                .partial_phase1_bw(strip, r.x, r.y, r.w, r.h, &mut delay, &draw)
                        }
                    });

                    if let Some(rs) = rs {
                        // phase 2: kick DU waveform (~400-600ms)
                        board.display.epd.partial_start_du(&rs);

                        // process input + work while DU runs
                        let deferred = busy_wait_with_input!(
                            board.display.epd,
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

                        // phase 3: sync RED+BW; skip if content changed during DU (rapid nav).
                        // draw() now produces the next page; writing it to both planes ghosts.
                        // leave RED stale; next render uses inv-red to correct it.
                        // merge region so the inv-red pass covers pixels this DU changed.
                        if launcher.ctx.has_redraw() {
                            // content changed; skip sync, mark region for inv-red pass
                            launcher.ctx.mark_dirty(r);
                            red_stale = true;
                            partial_refreshes += 1;
                        } else {
                            // stable; sync planes, power off
                            red_stale = false;
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
                            partial_refreshes += 1;
                            board.display.epd.power_off_async().await;
                        }

                        // apply deferred transition from busy wait
                        if let Some(transition) = deferred
                            && let Some(nav) = launcher.apply(transition)
                        {
                            apply_transition!(
                                nav, launcher, home, files, reader, settings, bm_cache, quick_menu,
                                bumps
                            );
                        }

                        break 'render;
                    }

                    if !board.display.epd.needs_initial_refresh() {
                        break 'render; // degenerate zero-size region
                    }
                    // fall through to full GC
                    info!("display: partial failed (initial refresh), promoting to full");
                } else {
                    info!("display: promoted partial to full (ghosting clear)");
                }
                // fall through to full GC
            }

            // full GC refresh: explicit Full, ghost-clear, or initial-refresh fallback
            if matches!(redraw, Redraw::Full | Redraw::Partial(_)) {
                // ensure analog off; no-op normally, required after skipped power-off
                board.display.epd.power_off_async().await;

                update_statusbar(statusbar, cached_battery_mv, sd_ok);

                let active = launcher.active();
                with_app!(active, home, files, reader, settings, |app| {
                    board.display.epd.write_full_frame(
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
                    );
                });

                board.display.epd.start_full_update();

                // process input during ~1.6s GC waveform
                let deferred = busy_wait_with_input!(
                    board.display.epd,
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
                red_stale = false;

                if let Some(transition) = deferred
                    && let Some(nav) = launcher.apply(transition)
                {
                    apply_transition!(
                        nav, launcher, home, files, reader, settings, bm_cache, quick_menu, bumps
                    );
                }
            }
        } // 'render
    }
}

// helpers

fn update_statusbar(bar: &mut StatusBar, battery_mv: u16, sd_ok: bool) {
    const HEAP_TOTAL: usize = 143360; // matches heap_allocator!(size: ...) above
    let stats = esp_alloc::HEAP.stats();

    let bat_pct = battery::battery_percentage(battery_mv);

    bar.update(&SystemStatus {
        uptime_secs: uptime_secs(),
        battery_mv,
        battery_pct: bat_pct,
        heap_used: stats.current_usage,
        heap_peak: stats.max_usage,
        heap_total: HEAP_TOTAL,
        stack_free: free_stack_bytes(),
        stack_hwm: stack_high_water_mark(),
        sd_ok,
    });
}

// push quick-menu cycle changes into active app; persist settings-owned values
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
                AppId::Upload => {}
            }
        }
    }

    // persist reader font-size change into settings
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
