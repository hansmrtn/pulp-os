// scheduler: main event loop, render pipeline, housekeeping, sleep
//
// EPD and SD share a single SPI bus via CriticalSectionDevice;
// during normal operation, all SD I/O completes before render()
// touches the EPD; during the DU/GC waveform (~400ms), the EPD
// charge pump drives pixels with no SPI commands, so the bus is
// free for SD I/O - busy_wait_with_background exploits this
// window to run background caching and housekeeping
//
// handle_input and poll_housekeeping are synchronous; they return
// a bool flag when the caller should enter_sleep (which is async
// because it renders a sleep screen via the EPD)
//
// sd_card_sleep sends cmd0 before deep sleep to reduce sd card
// idle current from ~150 uA to ~10 uA

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Ticker, with_timeout};
use log::info;

use super::app::{AppLayer, Redraw, Transition};
use crate::board::button::Button;
use crate::drivers::battery;
use crate::drivers::input::Event;
use crate::drivers::strip::StripBuffer;
use crate::kernel::tasks;

use crate::ui::{free_stack_bytes, stack_high_water_mark};

use super::timing;

#[inline]
fn is_power_event(ev: Event) -> bool {
    matches!(ev, Event::Press(Button::Power) | Event::Release(Button::Power))
}

impl super::Kernel {
    // render boot console to EPD; call before boot() to show
    // hardware init progress in the built-in mono font
    pub async fn show_boot_console(&mut self, console: &super::BootConsole) {
        let draw = |s: &mut StripBuffer| console.draw(s);
        self.epd
            .full_refresh_async(self.strip, &mut self.delay, &draw)
            .await;
    }

    // one-time boot: load caches, settings, render the home screen
    // if waking from deep sleep with valid RTC session, restore it
    pub async fn boot<A: AppLayer>(&mut self, app_mgr: &mut A) {
        use super::rtc_session;

        self.bm_cache.ensure_loaded(&self.sd);

        // check for valid RTC session before loading settings
        // (session may contain cached settings to skip SD reads)
        let has_rtc_session = rtc_session::is_valid_session();
        let rtc_session_data = if has_rtc_session {
            let session = rtc_session::load();
            info!(
                "boot: RTC session valid (wake count {})",
                session.wake_count()
            );
            Some(session)
        } else {
            info!("boot: no RTC session (power-on or first boot)");
            None
        };

        // load settings - may use cached values from RTC session
        {
            let mut handle = self.handle();
            app_mgr.load_eager_settings(&mut handle);
            app_mgr.load_initial_state(&mut handle);
        }

        tasks::set_idle_timeout(app_mgr.system_settings().sleep_timeout);
        self.log_stats();

        // try to restore session from RTC memory
        let restored = if let Some(session) = rtc_session_data {
            app_mgr.apply_session(&session, &mut self.handle())
        } else {
            false
        };

        if !restored {
            app_mgr.enter_initial(&mut self.handle());
        }

        {
            let draw = |s: &mut StripBuffer| app_mgr.draw(s);
            self.epd
                .full_refresh_async(self.strip, &mut self.delay, &draw)
                .await;
        }
        let _ = app_mgr.take_redraw();

        info!("ui ready.");
    }

    // event-driven main loop; never returns
    //
    // two genuine async suspension points in steady state:
    //   1. select(INPUT_EVENTS.receive(), work_ticker.next())
    //   2. EPD busy pin wait inside render()
    // everything between them is synchronous function calls
    pub async fn run<A: AppLayer>(&mut self, app_mgr: &mut A) -> ! {
        let mut work_ticker = Ticker::every(Duration::from_millis(timing::TICK_MS));

        loop {
            if app_mgr.needs_special_mode() {
                self.handle_special_mode(app_mgr).await;
                continue;
            }

            // async point 1: wait for input or tick
            let hw_event = match select(tasks::INPUT_EVENTS.receive(), work_ticker.next()).await {
                Either::First(ev) => Some(ev),
                Either::Second(_) => None,
            };

            if let Some(ev) = hw_event {
                if matches!(ev, Event::LongPress(_)) {
                    info!("scheduler: received {:?}", ev);
                }
                if self.handle_input(ev, app_mgr) {
                    self.sleep_with_session(app_mgr, "power held").await;
                    continue;
                }
            }

            if app_mgr.needs_special_mode() {
                continue;
            }

            // SPI bus sharing invariant
            //
            // the EPD and SD card share a single SPI2 bus via
            // CriticalSectionDevice (RefCell under the hood).
            //
            //   1. background SD I/O runs here, before EPD access;
            //      interruptible by input so the user can navigate
            //      away during long-running caching operations
            //   2. poll_housekeeping may do SD I/O, also before render
            //   3. render() touches the EPD; during the waveform window
            //      busy_wait_with_background runs SD I/O because the
            //      EPD charge pump is driving pixels with no SPI commands
            //   4. no SD I/O outside these three sites
            //
            // when input arrives during run_background, the background
            // future is dropped. this is safe: partial chapter cache
            // writes leave ch_cached=false so the chapter is recached
            // on the next attempt
            let bg_input = {
                let mut handle = self.handle();
                match select(
                    app_mgr.run_background(&mut handle),
                    tasks::INPUT_EVENTS.receive(),
                )
                .await
                {
                    Either::First(()) => None,
                    Either::Second(ev) => Some(ev),
                }
            };

            if let Some(ev) = bg_input {
                if self.handle_input(ev, app_mgr) {
                    self.sleep_with_session(app_mgr, "power held").await;
                    continue;
                }

                if app_mgr.needs_special_mode() {
                    continue;
                }
            }

            if self.poll_housekeeping(app_mgr) {
                self.sleep_with_session(app_mgr, "idle timeout").await;
                continue;
            }

            if app_mgr.ctx_mut().render_ready() {
                let redraw = app_mgr.take_redraw();
                if self.render(app_mgr, redraw).await {
                    self.sleep_with_session(app_mgr, "power held").await;
                    continue;
                }
            }
        }
    }

    // delegate to app layer for modes that bypass normal dispatch
    // (e.g. wifi upload); kernel passes hardware resources through
    async fn handle_special_mode<A: AppLayer>(&mut self, app_mgr: &mut A) {
        app_mgr
            .run_special_mode(&mut self.epd, self.strip, &mut self.delay, &self.sd)
            .await;

        app_mgr.apply_transition(Transition::Pop, &mut self.handle());
        app_mgr.request_full_redraw();
    }

    // returns true if caller should call enter_sleep
    //
    // note: the original async version called enter_sleep inline
    // on power-long-press and then fell through to dispatch_event
    // if sleep_deep somehow returned; this version correctly returns
    // early so the caller can enter_sleep and continue the loop
    fn handle_input<A: AppLayer>(&mut self, hw_event: Event, app_mgr: &mut A) -> bool {
        let _ = tasks::IDLE_SLEEP_DUE.try_take();

        if hw_event == Event::LongPress(Button::Power) {
            info!("handle_input: LongPress(Power) detected, triggering sleep");
            return true;
        }

        let suppressed_before = app_mgr.suppress_deferred_input();
        let transition = app_mgr.dispatch_event(hw_event, &mut *self.bm_cache);
        let power = is_power_event(hw_event);

        if transition != Transition::None {
            app_mgr.apply_transition(transition, &mut self.handle());
            // don't consume hold for power button - we still want LongPress for sleep
            if !power {
                tasks::request_hold_reset();
            }
        } else if app_mgr.suppress_deferred_input() != suppressed_before {
            // quick menu opened/closed; don't consume hold for power button
            if !power {
                tasks::request_hold_reset();
            }
        }

        false
    }

    // shared housekeeping body: battery, sd probe, bookmark flush, stats
    fn poll_housekeeping_inner<A: AppLayer>(&mut self, app_mgr: &A) {
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
    }

    // returns true if idle sleep is due
    fn poll_housekeeping<A: AppLayer>(&mut self, app_mgr: &A) -> bool {
        self.poll_housekeeping_inner(app_mgr);
        tasks::IDLE_SLEEP_DUE.try_take().is_some()
    }

    // housekeeping without idle-sleep check; never sleep mid-refresh
    fn poll_housekeeping_waveform<A: AppLayer>(&mut self, app_mgr: &A) {
        self.poll_housekeeping_inner(app_mgr);
    }

    // partial refreshes use DU waveform (~400 ms); after ghost_clear_every
    // partials, a full GC refresh (~1.6 s) clears ghosting
    //
    // returns true if power-long-press arrived during the waveform and
    // the caller should enter sleep
    async fn render<A: AppLayer>(&mut self, app_mgr: &mut A, redraw: Redraw) -> bool {
        let mut sleep_requested = false;

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
                        let (deferred, sleep) = self.busy_wait_with_background(app_mgr).await;
                        sleep_requested = sleep;

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
                            app_mgr.apply_transition(transition, &mut self.handle());
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

                let (deferred, sleep) = self.busy_wait_with_background(app_mgr).await;
                sleep_requested = sleep;

                self.epd.finish_full_update();
                self.partial_refreshes = 0;
                self.red_stale = false;

                if let Some(transition) = deferred {
                    app_mgr.apply_transition(transition, &mut self.handle());
                }
            }
        } // 'render

        sleep_requested
    }

    // collect input and run background work while EPD is busy refreshing
    //
    // during the DU/GC waveform the EPD charge pump drives pixels;
    // no SPI commands are sent, so the bus is free for SD I/O.
    // is_busy() is a sync GPIO read; no epd borrow is held across
    // any .await point, so self is fully available for handle() etc.
    //
    // run_background is wrapped in select so input interrupts long
    // background work (e.g. chapter caching). when the background
    // future is dropped mid-stream, partial cache writes are safe
    // because ch_cached stays false until the full write completes.
    // the TICK_MS timeout ensures is_busy is re-checked regularly
    // even during long background operations.
    //
    // first non-None transition wins; hold reset prevents the held
    // button from re-firing LongPress/Repeat for the waveform
    //
    // returns (deferred_transition, sleep_requested) so the caller
    // can enter sleep after the EPD finishes if power-long-press
    // arrived during the waveform
    async fn busy_wait_with_background<A: AppLayer>(
        &mut self,
        app_mgr: &mut A,
    ) -> (Option<Transition<A::Id>>, bool) {
        let mut deferred: Option<Transition<A::Id>> = None;
        let mut sleep_requested = false;

        loop {
            if !self.epd.is_busy() {
                break;
            }

            // run background, interruptible by input or tick timeout
            let ev = {
                let mut handle = self.handle();
                match select(
                    app_mgr.run_background(&mut handle),
                    with_timeout(
                        Duration::from_millis(timing::TICK_MS),
                        tasks::INPUT_EVENTS.receive(),
                    ),
                )
                .await
                {
                    Either::First(()) => None,
                    Either::Second(Ok(ev)) => Some(ev),
                    Either::Second(Err(_)) => None,
                }
            };

            if let Some(hw_event) = ev {
                let _ = tasks::IDLE_SLEEP_DUE.try_take();

                // power long-press triggers sleep after EPD finishes
                if hw_event == Event::LongPress(Button::Power) {
                    info!("busy_wait: LongPress(Power) during waveform, will sleep after");
                    sleep_requested = true;
                    continue;
                }

                let suppressed_before = app_mgr.suppress_deferred_input();
                if !suppressed_before {
                    let t = app_mgr.dispatch_event(hw_event, &mut *self.bm_cache);
                    let power = is_power_event(hw_event);

                    if t != Transition::None && deferred.is_none() {
                        deferred = Some(t);
                        if !power {
                            tasks::request_hold_reset();
                        }
                    } else if app_mgr.suppress_deferred_input() != suppressed_before {
                        if !power {
                            tasks::request_hold_reset();
                        }
                    }
                }
            }

            self.poll_housekeeping_waveform(app_mgr);
        }

        (deferred, sleep_requested)
    }

    // save session to RTC memory and enter deep sleep; call this
    // instead of enter_sleep directly to ensure session state is persisted
    async fn sleep_with_session<A: AppLayer>(&mut self, app_mgr: &mut A, reason: &str) {
        use super::rtc_session;

        // collect session state from app layer
        let mut session = rtc_session::RtcSession::zeroed();
        app_mgr.collect_session(&mut session);

        // increment wake count for debugging
        session.increment_wake_count();

        // save to RTC memory (will be valid on next wake)
        rtc_session::save(&session);
        info!("session: saved to RTC memory");

        self.enter_sleep(reason).await;
    }

    // flush bookmarks, render sleep screen, enter MCU deep sleep;
    // on real hardware this never returns (wake = full MCU reset)
    //
    // uses a custom sleep config that keeps RTC FAST memory powered
    // so session state survives the sleep cycle (~1-2µA extra)
    async fn enter_sleep(&mut self, reason: &str) {
        use embedded_graphics::mono_font::MonoTextStyle;
        use embedded_graphics::mono_font::ascii::FONT_9X18;
        use embedded_graphics::pixelcolor::BinaryColor;
        use embedded_graphics::prelude::*;
        use embedded_graphics::text::Text;
        use esp_hal::gpio::RtcPinWithResistors;
        use esp_hal::rtc_cntl::Rtc;
        use esp_hal::rtc_cntl::sleep::{RtcSleepConfig, RtcioWakeupSource, WakeupLevel};

        info!("{}: entering sleep...", reason);

        if self.bm_cache.is_dirty() {
            self.bm_cache.flush(&self.sd);
        }

        self.sd_card_sleep();

        self.epd
            .full_refresh_async(self.strip, &mut self.delay, &|s: &mut StripBuffer| {
                let style = MonoTextStyle::new(&FONT_9X18, BinaryColor::On);
                let _ = Text::new("(sleep)", Point::new(210, 400), style).draw(s);
            })
            .await;
        info!("display: sleep screen rendered");

        self.epd.enter_deep_sleep();
        info!("display: deep sleep mode 1");

        // safety: deep sleep never returns, the MCU resets on wake, so
        // these stolen peripherals cannot alias with their original
        // owners. LPWR is not used elsewhere; GPIO3 was previously
        // cloned into InputHw but we are about to halt the CPU
        let mut rtc = Rtc::new(unsafe { esp_hal::peripherals::LPWR::steal() });
        let mut gpio3 = unsafe { esp_hal::peripherals::GPIO3::steal() };
        let wakeup_pins: &mut [(&mut dyn RtcPinWithResistors, WakeupLevel)] =
            &mut [(&mut gpio3, WakeupLevel::Low)];
        let rtcio = RtcioWakeupSource::new(wakeup_pins);

        // custom sleep config: keep RTC FAST memory powered for session
        // persistence. this adds ~1-2µA to deep sleep current but enables
        // instant wake restoration without SD card I/O.
        let mut sleep_config = RtcSleepConfig::deep();
        sleep_config.set_rtc_fastmem_pd_en(false); // keep RTC FAST powered

        info!("mcu: entering deep sleep (power button to wake, RTC FAST retained)");
        rtc.sleep(&sleep_config, &[&rtcio]);

        // deep sleep resets the MCU; backstop if sleep returns
        #[allow(unreachable_code)]
        loop {
            core::hint::spin_loop();
        }
    }

    // send cmd0 to put sd card into idle/sleep state;
    // reduces sd current from ~150 µa to ~10 µa during deep sleep.
    // call after all sd i/o is done and before epd sleep-screen render
    fn sd_card_sleep(&self) {
        use embedded_hal::digital::OutputPin;

        self.sd.flush_and_close();

        critical_section::with(|cs| {
            let bus_ref = crate::board::SPI_BUS_REF.borrow(cs).get();
            let mut cs_pin = crate::board::SD_CS_SLEEP.borrow_ref_mut(cs);

            if let (Some(bus_ref), Some(pin)) = (bus_ref, cs_pin.as_mut()) {
                let mut bus: core::cell::RefMut<'_, _> = bus_ref.borrow(cs).borrow_mut();
                // 80 clocks cs high (sd spec: card ready for command)
                let _ = bus.write(&[0xFF; 10]);
                let _ = pin.set_low();
                // cmd0 (GO_IDLE_STATE) with valid crc
                let _ = bus.write(&[0x40, 0x00, 0x00, 0x00, 0x00, 0x95]);
                let _ = bus.write(&[0xFF]);
                let _ = pin.set_high();
            }
        });
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
