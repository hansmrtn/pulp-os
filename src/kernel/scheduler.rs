// scheduler: main event loop, render pipeline, housekeeping, sleep
//
// EPD and SD share a single SPI bus via CriticalSectionDevice;
// busy_wait_with_input() does NOT run background SD I/O while
// the EPD is refreshing to avoid RefCell borrow conflicts

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Ticker, Timer};
use log::info;

use crate::apps::manager::AppManager;
use crate::apps::{AppId, Redraw, Transition};
use crate::board::button::Button;
use crate::drivers::battery;
use crate::drivers::input::Event;
use crate::drivers::strip::StripBuffer;
use crate::kernel::tasks;

use crate::ui::{free_stack_bytes, stack_high_water_mark};

const TICK_MS: u64 = 10;

impl super::Kernel {
    // one-time boot: load caches, settings, render the home screen
    pub async fn boot(&mut self, app_mgr: &mut AppManager) {
        self.bm_cache.ensure_loaded(&self.sd);

        {
            let mut handle = self.handle();
            app_mgr.load_eager_settings(&mut handle);
            app_mgr.load_home_recent(&mut handle);
        }

        tasks::set_idle_timeout(app_mgr.system_settings().sleep_timeout);
        self.log_stats();
        app_mgr.enter_initial(&mut self.handle()).await;

        {
            let draw = |s: &mut StripBuffer| app_mgr.draw(s);
            self.epd
                .full_refresh_async(self.strip, &mut self.delay, &draw)
                .await;
        }
        let _ = app_mgr.take_redraw();

        info!("ui ready.");
    }

    // event-driven main loop -- never returns
    pub async fn run(&mut self, app_mgr: &mut AppManager) -> ! {
        let mut work_ticker = Ticker::every(Duration::from_millis(TICK_MS));

        loop {
            if app_mgr.active() == AppId::Upload {
                self.run_upload_mode(app_mgr).await;
                continue;
            }

            let hw_event = match select(tasks::INPUT_EVENTS.receive(), work_ticker.next()).await {
                Either::First(ev) => Some(ev),
                Either::Second(_) => None,
            };

            if let Some(ev) = hw_event {
                self.handle_input(ev, app_mgr).await;
            }

            if app_mgr.active() == AppId::Upload {
                continue;
            }

            // SAFETY-CRITICAL: SPI bus sharing invariant
            //
            // The EPD and SD card share a single SPI2 bus via
            // CriticalSectionDevice (RefCell under the hood).  SD I/O
            // and EPD rendering must NEVER overlap — concurrent access
            // would cause a RefCell borrow panic at runtime.
            //
            // This ordering enforces that:
            //   1. All background SD I/O (app caching, title scan, etc.)
            //      completes here, before any EPD access.
            //   2. poll_housekeeping may do SD I/O (bookmark flush,
            //      SD probe) — also before render.
            //   3. render() is the only code below that touches the EPD.
            //   4. busy_wait_with_input() does NOT run background work
            //      while the EPD is refreshing — only input collection.
            //
            // If you add new SD I/O call sites, they MUST go above the
            // render() call.  Violating this will panic, not corrupt.
            {
                let mut handle = self.handle();
                app_mgr.run_background(&mut handle).await;
            }

            self.poll_housekeeping(app_mgr).await;

            if app_mgr.has_redraw() {
                let redraw = app_mgr.take_redraw();
                self.render(app_mgr, redraw).await;
            }
        }
    }

    async fn run_upload_mode(&mut self, app_mgr: &mut AppManager) {
        // Safety: WIFI is not owned by any other driver.  Upload mode
        // runs in isolation (the scheduler exits the main dispatch loop
        // first) and tears down the radio stack before returning.  The
        // peripheral is not accessed again until the next upload session.
        let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };

        crate::apps::upload::run_upload_mode(
            wifi,
            &mut self.epd,
            self.strip,
            &mut self.delay,
            &self.sd,
            app_mgr.system_settings().ui_font_size_idx,
            &*app_mgr.bumps,
            app_mgr.wifi_config(),
        )
        .await;

        app_mgr
            .apply_transition(Transition::Pop, &mut self.handle())
            .await;
        app_mgr.request_full_redraw();
    }

    async fn handle_input(&mut self, hw_event: Event, app_mgr: &mut AppManager) {
        // power long-press -> sleep (intercept before app dispatch)
        if hw_event == Event::LongPress(Button::Power) {
            self.enter_sleep("power held").await;
        }

        let transition = app_mgr.dispatch_event(hw_event, &mut *self.bm_cache);

        if transition != Transition::None {
            app_mgr
                .apply_transition(transition, &mut self.handle())
                .await;
        }
    }

    async fn poll_housekeeping(&mut self, app_mgr: &AppManager) {
        if let Some(mv) = tasks::BATTERY_MV.try_take() {
            self.cached_battery_mv = mv;
        }

        if tasks::SD_CHECK_DUE.try_take().is_some() {
            self.sd_ok = self.sd.probe_ok();
        }

        if tasks::BOOKMARK_FLUSH_DUE.try_take().is_some() && self.bm_cache.is_dirty() {
            self.bm_cache.flush(&self.sd);
        }

        if tasks::STATUS_DUE.try_take().is_some() {
            self.log_stats();
            if app_mgr.settings_loaded() {
                tasks::set_idle_timeout(app_mgr.system_settings().sleep_timeout);
            }
        }

        if tasks::IDLE_SLEEP_DUE.try_take().is_some() {
            self.enter_sleep("idle timeout").await;
        }
    }

    // partial refreshes use DU waveform (~400 ms); after ghost_clear_every
    // partials, a full GC refresh (~1.6 s) clears ghosting
    async fn render(&mut self, app_mgr: &mut AppManager, redraw: Redraw) {
        'render: {
            if let Redraw::Partial(r) = redraw {
                let ghost_clear_every = app_mgr.ghost_clear_every();

                if self.partial_refreshes < ghost_clear_every {
                    let r = r.align8();

                    let rs = {
                        let draw = |s: &mut StripBuffer| app_mgr.draw(s);
                        if self.red_stale {
                            self.epd.partial_phase1_bw_inv_red(
                                self.strip,
                                r.x,
                                r.y,
                                r.w,
                                r.h,
                                &mut self.delay,
                                &draw,
                            )
                        } else {
                            self.epd.partial_phase1_bw(
                                self.strip,
                                r.x,
                                r.y,
                                r.w,
                                r.h,
                                &mut self.delay,
                                &draw,
                            )
                        }
                    };

                    if let Some(rs) = rs {
                        self.epd.partial_start_du(&rs);
                        let deferred = self.busy_wait_with_input(app_mgr).await;

                        if app_mgr.has_redraw() {
                            // content changed mid-DU; leave RED stale
                            app_mgr.ctx_mut().mark_dirty(r);
                            self.red_stale = true;
                            self.partial_refreshes += 1;
                        } else {
                            self.red_stale = false;
                            {
                                let draw = |s: &mut StripBuffer| app_mgr.draw(s);
                                self.epd.partial_phase3_sync(self.strip, &rs, &draw);
                            }
                            self.partial_refreshes += 1;
                            self.epd.power_off_async().await;
                        }

                        if let Some(transition) = deferred {
                            app_mgr
                                .apply_transition(transition, &mut self.handle())
                                .await;
                        }

                        break 'render;
                    }

                    if !self.epd.needs_initial_refresh() {
                        break 'render;
                    }
                    info!("display: partial failed (initial refresh), promoting to full");
                } else {
                    info!("display: promoted partial to full (ghosting clear)");
                }
            }

            if matches!(redraw, Redraw::Full | Redraw::Partial(_)) {
                self.epd.power_off_async().await;

                self.log_stats();

                {
                    let draw = |s: &mut StripBuffer| app_mgr.draw(s);
                    self.epd
                        .write_full_frame(self.strip, &mut self.delay, &draw);
                }

                self.epd.start_full_update();

                let deferred = self.busy_wait_with_input(app_mgr).await;

                self.epd.finish_full_update();
                self.partial_refreshes = 0;
                self.red_stale = false;

                if let Some(transition) = deferred {
                    app_mgr
                        .apply_transition(transition, &mut self.handle())
                        .await;
                }
            }
        } // 'render
    }

    // Collect input events while EPD is busy refreshing.
    //
    // SAFETY-CRITICAL: no SD I/O or background work may run here.
    // The EPD is actively driving the SPI bus during refresh; any
    // SD access would cause a RefCell borrow panic.  Only input
    // events (from the ADC-based input_task) are collected.
    async fn busy_wait_with_input(&mut self, app_mgr: &mut AppManager) -> Option<Transition> {
        let mut deferred: Option<Transition> = None;

        loop {
            if !self.epd.is_busy() {
                break;
            }

            match select(
                self.epd.busy_pin().wait_for_low(),
                select(
                    tasks::INPUT_EVENTS.receive(),
                    Timer::after(Duration::from_millis(TICK_MS)),
                ),
            )
            .await
            {
                Either::First(_) => break,

                Either::Second(Either::First(hw_event)) => {
                    if app_mgr.quick_menu.open {
                        continue;
                    }

                    let t = app_mgr.dispatch_event(hw_event, &mut *self.bm_cache);
                    if t != Transition::None && deferred.is_none() {
                        deferred = Some(t);
                    }
                }

                Either::Second(Either::Second(_)) => {}
            }
        }

        deferred
    }

    // flush bookmarks, render sleep screen, enter MCU deep sleep
    // on real hardware this never returns (wake = full MCU reset)
    pub async fn enter_sleep(&mut self, reason: &str) {
        use embedded_graphics::mono_font::MonoTextStyle;
        use embedded_graphics::mono_font::ascii::FONT_6X13;
        use embedded_graphics::pixelcolor::BinaryColor;
        use embedded_graphics::prelude::*;
        use embedded_graphics::text::Text;
        use esp_hal::gpio::RtcPinWithResistors;
        use esp_hal::rtc_cntl::Rtc;
        use esp_hal::rtc_cntl::sleep::{RtcioWakeupSource, WakeupLevel};

        info!("{}: entering sleep...", reason);

        if self.bm_cache.is_dirty() {
            self.bm_cache.flush(&self.sd);
        }

        self.epd
            .full_refresh_async(self.strip, &mut self.delay, &|s: &mut StripBuffer| {
                let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::On);
                let _ = Text::new("(sleep)", Point::new(210, 400), style).draw(s);
            })
            .await;
        info!("display: sleep screen rendered");

        self.epd.enter_deep_sleep();
        info!("display: deep sleep mode 1");

        // Safety: deep sleep never returns — the MCU resets on wake, so
        // these stolen peripherals cannot alias with their original
        // owners.  LPWR is not used elsewhere; GPIO3 was previously
        // cloned into InputHw but we are about to halt the CPU.
        let mut rtc = Rtc::new(unsafe { esp_hal::peripherals::LPWR::steal() });
        let mut gpio3 = unsafe { esp_hal::peripherals::GPIO3::steal() };
        let wakeup_pins: &mut [(&mut dyn RtcPinWithResistors, WakeupLevel)] =
            &mut [(&mut gpio3, WakeupLevel::Low)];
        let rtcio = RtcioWakeupSource::new(wakeup_pins);

        info!("mcu: entering deep sleep (power button to wake)");
        rtc.sleep_deep(&[&rtcio]);

        // deep sleep resets the MCU; backstop if sleep_deep returns
        #[allow(unreachable_code)]
        loop {
            core::hint::spin_loop();
        }
    }

    pub fn log_stats(&self) {
        let stats = esp_alloc::HEAP.stats();
        let bat_pct = battery::battery_percentage(self.cached_battery_mv);
        let uptime = super::uptime_secs();
        let mins = (uptime / 60) % 60;
        let hrs = uptime / 3600;

        info!(
            "stats: heap {}/{}K peak {}K | stack free {}K hwm {}K | bat {}% {}.{}V | up {}:{:02} | SD:{}",
            stats.current_usage / 1024,
            stats.size / 1024,
            stats.max_usage / 1024,
            free_stack_bytes() / 1024,
            stack_high_water_mark() / 1024,
            bat_pct,
            self.cached_battery_mv / 1000,
            (self.cached_battery_mv % 1000) / 100,
            hrs,
            mins,
            if self.sd_ok { "ok" } else { "--" },
        );
    }
}
