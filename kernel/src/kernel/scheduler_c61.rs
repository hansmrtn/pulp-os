// OnePage C61 half of the scheduler (T12): boot, render, card detect, battery
// and deep-sleep entry. The event loop itself (scheduler.rs `run`,
// `handle_input`, `poll_housekeeping`) is shared with the X4.
//
// What differs from the X4 on purpose:
//   * every refresh is a FULL refresh (partial refresh is out of scope for the
//     first version). `Redraw::Partial` is promoted, so the panel flashes on
//     each page turn, and `ghost_clear_every` has no effect;
//   * `Epd::full_refresh` blocks until the controller is idle (BUSY bounded at
//     5 s). There is no `busy_wait_with_background`, so no input is collected
//     and no background work runs during a refresh; events queue in
//     `INPUT_EVENTS` and are served right after;
//   * a failed refresh follows `pulp_board_logic::lifecycle::DisplayHealth`
//     (re-init and retry once, then give up for this frame and redraw on the
//     next input). The panel cannot show its own error, so there is no screen
//     for it: the failure is in the log;
//   * the session is on the SD card (two slot files) and goes through
//     `board_c61::sleep::enter_deep_sleep`, never through `begin_shutdown` /
//     `cut_peripheral_power` directly;
//   * SD card detect (GPIO28) and the battery monitor (GPIO5/10) are polled
//     here, on the main loop, because the sleep sequence needs `&mut` access to
//     the same parts (see `board_c61::hw`).
//
// Not verified on hardware: all of it.

use embassy_time::Instant;
use log::{error, info, warn};

use pulp_board_logic::lifecycle::{
    DisplayHealth, PostRestore, Refresher, RestoreEnv, check_restorable, post_restore, run_refresh,
};
use pulp_board_logic::power::PeripheralPower;
use pulp_board_logic::session::SessionState;
use pulp_board_logic::sleep::BootPlan;
use pulp_board_logic::ssd1677::{Rotation, StripSource};

use super::app::{AppLayer, Redraw};
use super::tasks;
use crate::board_c61::epd::Epd;
use crate::board_c61::power::Gpio27Rail;
use crate::board_c61::sd::{self, CardEvent};
use crate::board_c61::session::{self, SdSessionStore};
use crate::board_c61::sleep::{self, StoreSaver, WakeCause};
use crate::drivers::sdcard::SdStorage;
use crate::drivers::storage;
use crate::drivers::strip::StripBuffer;

#[inline]
fn now_ms() -> u64 {
    Instant::now().as_millis()
}

// feeds the SSD1677 driver one strip at a time from the app's `draw`
struct AppStrips<'a, A: AppLayer> {
    strip: &'a mut StripBuffer,
    app: &'a A,
}

impl<A: AppLayer> StripSource for AppStrips<'_, A> {
    fn render_strip(&mut self, rotation: Rotation, idx: u16) -> &[u8] {
        self.strip.begin_strip(rotation, idx);
        self.app.draw(self.strip);
        self.strip.data()
    }
}

// the sleep screen, same text and position as the X4
fn draw_sleep_screen(strip: &mut StripBuffer) {
    use embedded_graphics::mono_font::MonoTextStyle;
    use embedded_graphics::mono_font::ascii::FONT_9X18;
    use embedded_graphics::pixelcolor::BinaryColor;
    use embedded_graphics::prelude::*;
    use embedded_graphics::text::Text;

    let style = MonoTextStyle::new(&FONT_9X18, BinaryColor::On);
    let _ = Text::new("(sleep)", Point::new(210, 400), style).draw(strip);
}

struct FnStrips<'a, F: Fn(&mut StripBuffer)> {
    strip: &'a mut StripBuffer,
    draw: F,
}

impl<F: Fn(&mut StripBuffer)> StripSource for FnStrips<'_, F> {
    fn render_strip(&mut self, rotation: Rotation, idx: u16) -> &[u8] {
        self.strip.begin_strip(rotation, idx);
        (self.draw)(self.strip);
        self.strip.data()
    }
}

// controller init through the GPIO27 policy (software reset only); false when
// it failed (the state machine refused, or BUSY / bus error). Free functions
// over the individual fields so a strip source can borrow `Kernel::strip` at
// the same time.
fn reinit_display(epd: &mut Epd, power: &mut PeripheralPower<Gpio27Rail>) -> bool {
    let reset = match power.display_reset() {
        Ok(r) => r,
        Err(e) => {
            error!("display: reset refused: {:?}", e);
            return false;
        }
    };
    match epd.init(reset) {
        Ok(()) => true,
        Err(e) => {
            error!("display: init failed: {}", e.as_str());
            false
        }
    }
}

// Hardware side of one refresh attempt for `run_refresh`: the driver, the GPIO27
// policy (for the re-init token) and the strip source, borrowed as separate
// fields so the source can read `Kernel::strip`.
struct EpdRefresher<'a, S: StripSource> {
    epd: &'a mut Epd,
    power: &'a mut PeripheralPower<Gpio27Rail>,
    src: &'a mut S,
}

impl<S: StripSource> Refresher for EpdRefresher<'_, S> {
    fn refresh(&mut self) -> bool {
        match self.epd.full_refresh(self.src) {
            Ok(()) => true,
            Err(e) => {
                warn!("display: full refresh failed: {}", e.as_str());
                false
            }
        }
    }

    fn reinit(&mut self) -> bool {
        reinit_display(self.epd, self.power)
    }
}

// Run one frame through `src` with the recovery policy. true = the panel shows
// the new frame; false = given up (logged), redrawn on the next input.
fn refresh_with_recovery<S: StripSource>(
    epd: &mut Epd,
    power: &mut PeripheralPower<Gpio27Rail>,
    health: &mut DisplayHealth,
    src: &mut S,
) -> bool {
    let shown = run_refresh(health, &mut EpdRefresher { epd, power, src });
    if !shown {
        error!("display: giving up on this frame, redraw on next input");
    }
    shown
}

impl super::Kernel {
    // one-time boot: load caches and settings, restore the saved position if
    // there is a valid and applicable one, render the first screen
    pub async fn boot<A: AppLayer>(&mut self, app_mgr: &mut A, wake: WakeCause) {
        self.bm_cache.ensure_loaded(&self.sd);

        // R20: decide before anything else touches the session files
        let plan = sleep::plan_boot(wake, session::restore_session(&self.hw.power, &self.sd));

        {
            let mut handle = self.handle();
            app_mgr.load_eager_settings(&mut handle);
            app_mgr.load_initial_state(&mut handle);
        }

        tasks::set_idle_timeout(app_mgr.system_settings().sleep_timeout);
        self.log_stats();

        let restored = match plan {
            BootPlan::Restore { cause, restored } => {
                info!(
                    "boot: session restore (cause {:?}, slot {:?} seq {})",
                    cause, restored.slot, restored.seq
                );
                self.restore_session(app_mgr, &restored.state)
            }
            BootPlan::Normal { cause, reason } => {
                info!("boot: normal boot (cause {:?}, {:?})", cause, reason);
                false
            }
        };

        if !restored {
            app_mgr.enter_initial(&mut self.handle());
        }

        if !reinit_display(&mut self.epd, &mut self.hw.power) {
            // not fatal: the first render re-initialises again
            self.hw.display.on_failure(1);
        }
        self.render_full(app_mgr);
        let _ = app_mgr.take_redraw();

        info!("ui ready.");
    }

    // R20 decision 3 (baseline): a validated record is applied if this firmware
    // and this card can show it; the record is kept after a successful apply
    // and deleted when it cannot be applied, so bad data cannot loop at boot
    fn restore_session<A: AppLayer>(&mut self, app_mgr: &mut A, state: &SessionState) -> bool {
        let env = RestoreEnv {
            upload_available: false, // the C61 firmware is offline-only
        };
        let sd = &self.sd;
        let applicable = check_restorable(state, env, |name| {
            core::str::from_utf8(name)
                .ok()
                .is_some_and(|n| storage::file_size(sd, n).is_ok())
        });
        let applied = match applicable {
            Ok(()) => {
                self.hw.wake_count = state.wake_count;
                app_mgr.apply_session(state, &mut self.handle())
            }
            Err(why) => {
                warn!("boot: saved session not applicable: {:?}", why);
                false
            }
        };
        match post_restore(applied) {
            PostRestore::Keep => {}
            PostRestore::ClearAndBootNormally => {
                warn!("boot: deleting the saved session, booting to Home");
                if let Err(e) = session::clear_session(&self.hw.power, &self.sd) {
                    warn!("boot: clearing the saved session failed: {:?}", e);
                }
            }
        }
        applied
    }

    // all redraws are full refreshes; returns false (never "sleep requested":
    // the C61 has no Power key)
    pub(super) async fn render<A: AppLayer>(&mut self, app_mgr: &mut A, redraw: Redraw) -> bool {
        if matches!(redraw, Redraw::None) {
            return false;
        }
        self.log_stats();
        self.render_full(app_mgr);
        self.partial_refreshes = 0;
        // the refresh blocked the executor: let the input / housekeeping tasks
        // run before the loop continues
        embassy_futures::yield_now().await;
        false
    }

    fn render_full<A: AppLayer>(&mut self, app: &A) {
        let mut src = AppStrips {
            strip: &mut *self.strip,
            app,
        };
        let _ = refresh_with_recovery(
            &mut self.epd,
            &mut self.hw.power,
            &mut self.hw.display,
            &mut src,
        );
    }

    // card detect (R9) and the 30 s battery measurement; called every loop
    // iteration, cheap when nothing is due
    pub(super) async fn poll_card<A: AppLayer>(&mut self, app_mgr: &mut A) {
        if !self.hw.card_due.due(now_ms()) {
            return;
        }
        let level = self.hw.card.pin.level_high();
        match self.hw.card.detect.sample(level) {
            Some(CardEvent::Removed) => {
                self.hw.card.health.on_card_event(CardEvent::Removed);
                // dropping the volume discards FAT state: a file that was open
                // for writing is not flushed (nothing can be written anyway)
                self.sd = SdStorage::empty();
                let _ = self.hw.power.card_removed();
                self.sd_ok = false;
                self.dir_cache.invalidate();
                warn!(
                    "sd: card removed ({})",
                    self.hw.card.health.status().message()
                );
                app_mgr.request_full_redraw();
            }
            Some(CardEvent::Inserted) => {
                if self.hw.card.health.on_card_event(CardEvent::Inserted) {
                    if self.hw.card.control.slow_down().is_err() {
                        warn!("sd: could not restore the 400 kHz probe clock");
                    }
                    let up = sd::bring_up(
                        &mut self.hw.power,
                        self.hw.card.pin.state(),
                        self.hw.card.spi.clone(),
                    )
                    .await;
                    if self.hw.card.control.speed_up().is_err() {
                        warn!("sd: could not restore the operating clock");
                    }
                    self.sd = up.storage;
                    self.hw.card.health = up.health;
                    self.sd_ok = self.sd.probe_ok();
                    if self.sd_ok
                        && let Err(e) = storage::ensure_pulp_dir_async(&self.sd).await
                    {
                        warn!("sd: _PULP dir: {}", e);
                    }
                    self.dir_cache.invalidate();
                    info!(
                        "sd: card inserted ({})",
                        self.hw.card.health.status().message()
                    );
                    app_mgr.request_full_redraw();
                }
            }
            None => {}
        }
    }

    // blocking (~30 ms settle + 16 conversions) but only every 30 s; a failed
    // read keeps the previous value (never 0 mV)
    pub(super) fn poll_battery(&mut self) {
        if !self.hw.battery_due.due(now_ms()) {
            return;
        }
        let Some(monitor) = self.hw.battery.as_mut() else {
            return;
        };
        match monitor.measure() {
            Ok(r) => self.cached_battery_mv = r.cell_mv,
            Err(e) => warn!("battery: sample failed: {:?} (charging resumed)", e),
        }
    }

    // flush, sleep screen, then the T11 sequence. Returns only when the
    // sequence aborted before anything irreversible (wake key still held, rail
    // not in a shutdown-able state): the device is then fully usable, so the
    // frame is redrawn and the idle timer restarts
    pub(super) async fn sleep_with_session<A: AppLayer>(&mut self, app_mgr: &mut A, reason: &str) {
        info!("{}: entering sleep...", reason);

        if self.bm_cache.is_dirty() {
            self.bm_cache.flush(&self.sd);
        }

        let mut state = SessionState::home();
        app_mgr.collect_session(&mut state);
        state.wake_count = self.hw.wake_count.wrapping_add(1);

        // sleep screen (best effort; a failed refresh must not block sleeping)
        {
            let mut src = FnStrips {
                strip: &mut *self.strip,
                draw: draw_sleep_screen,
            };
            let _ = refresh_with_recovery(
                &mut self.epd,
                &mut self.hw.power,
                &mut self.hw.display,
                &mut src,
            );
        }

        let mut store = SdSessionStore::new(&self.sd);
        let hw = &mut self.hw;
        let abort = sleep::enter_deep_sleep(
            &mut hw.power,
            StoreSaver {
                store: &mut store,
                state: &state,
            },
            &mut self.epd,
            &self.sd,
            &mut hw.battery,
            &mut hw.sleep.lines,
            &mut hw.sleep.wake,
            &mut hw.sleep.entry,
        );

        // still awake
        warn!("sleep: not entered ({:?}), continuing", abort.reason);
        tasks::IDLE_RESET.signal(());
        app_mgr.request_full_redraw();
    }
}
