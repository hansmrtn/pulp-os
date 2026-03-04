// app lifecycle manager: nav stack, dispatch, font propagation, draw
//
// all dispatch is static (monomorphized via with_app!); no dyn, no vtable

use crate::apps::files::FilesApp;
use crate::apps::home::HomeApp;
use crate::apps::reader::ReaderApp;
use crate::apps::settings::SettingsApp;
use crate::apps::{App, AppContext, AppId, Launcher, PendingSetting, Redraw, Transition};
use esp_hal::delay::Delay;

use crate::apps::widgets::quick_menu::{MAX_APP_ACTIONS, QuickMenuResult};
use crate::apps::widgets::{ButtonFeedback, QuickMenu};
use crate::board::action::{Action, ActionEvent, ButtonMapper};
use crate::board::{Epd, SCREEN_H, SCREEN_W};
use crate::drivers::input::Event;
use crate::drivers::sdcard::SdStorage;
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::kernel::KernelHandle;
use crate::kernel::app::AppLayer;
use crate::kernel::bookmarks::BookmarkCache;
use crate::kernel::config::{SystemSettings, WifiConfig};
use crate::ui::Region;

// monomorphized dispatch from AppId to concrete app type
macro_rules! with_app {
    ($id:expr, $mgr:expr, |$app:ident| $body:expr) => {
        match $id {
            AppId::Home => {
                let $app = &mut *$mgr.home;
                $body
            }
            AppId::Files => {
                let $app = &mut *$mgr.files;
                $body
            }
            AppId::Reader => {
                let $app = &mut *$mgr.reader;
                $body
            }
            AppId::Settings => {
                let $app = &mut *$mgr.settings;
                $body
            }
            AppId::Upload => {
                unreachable!("Upload mode is handled outside the app dispatch loop");
            }
        }
    };
}

// shared-ref variant for read-only dispatch (draw, quick_actions)
macro_rules! with_app_ref {
    ($id:expr, $mgr:expr, |$app:ident| $body:expr) => {
        match $id {
            AppId::Home => {
                let $app = &*$mgr.home;
                $body
            }
            AppId::Files => {
                let $app = &*$mgr.files;
                $body
            }
            AppId::Reader => {
                let $app = &*$mgr.reader;
                $body
            }
            AppId::Settings => {
                let $app = &*$mgr.settings;
                $body
            }
            AppId::Upload => {
                unreachable!("Upload mode is handled outside the app dispatch loop");
            }
        }
    };
}

#[allow(unused_imports)]
pub(crate) use with_app;
#[allow(unused_imports)]
pub(crate) use with_app_ref;

pub struct AppManager {
    pub launcher: &'static mut Launcher,

    pub home: &'static mut HomeApp,
    pub files: &'static mut FilesApp,
    pub reader: &'static mut ReaderApp,
    pub settings: &'static mut SettingsApp,

    pub quick_menu: &'static mut QuickMenu,
    pub bumps: &'static mut ButtonFeedback,

    pub mapper: ButtonMapper,
}

impl AppManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        launcher: &'static mut Launcher,
        home: &'static mut HomeApp,
        files: &'static mut FilesApp,
        reader: &'static mut ReaderApp,
        settings: &'static mut SettingsApp,
        quick_menu: &'static mut QuickMenu,
        bumps: &'static mut ButtonFeedback,
        mapper: ButtonMapper,
    ) -> Self {
        Self {
            launcher,
            home,
            files,
            reader,
            settings,
            quick_menu,
            bumps,
            mapper,
        }
    }

    #[inline]
    pub fn active(&self) -> AppId {
        self.launcher.active()
    }

    #[inline]
    pub fn ctx(&self) -> &AppContext {
        &self.launcher.ctx
    }

    #[inline]
    pub fn ctx_mut(&mut self) -> &mut AppContext {
        &mut self.launcher.ctx
    }

    #[inline]
    pub fn has_redraw(&self) -> bool {
        self.launcher.ctx.has_redraw()
    }

    #[inline]
    pub fn take_redraw(&mut self) -> Redraw {
        self.launcher.ctx.take_redraw()
    }

    #[inline]
    pub fn request_full_redraw(&mut self) {
        self.launcher.ctx.request_full_redraw();
    }

    #[inline]
    pub fn apply_nav(&mut self, transition: Transition) -> Option<crate::apps::NavEvent> {
        self.launcher.apply(transition)
    }

    pub fn load_eager_settings(&mut self, k: &mut KernelHandle<'_>) {
        self.settings.load_eager(k);
        self.propagate_fonts();
    }

    pub fn load_home_recent(&mut self, k: &mut KernelHandle<'_>) {
        self.home.load_recent(k);
    }

    pub fn enter_initial(&mut self, k: &mut KernelHandle<'_>) {
        self.home.on_enter(&mut self.launcher.ctx, k);
    }

    // power-button long-press must be intercepted by the scheduler
    // before calling this method
    pub fn dispatch_event(&mut self, hw_event: Event, bm_cache: &mut BookmarkCache) -> Transition {
        let event = self.mapper.map_event(hw_event);

        if self.quick_menu.open {
            return self.handle_quick_menu(event, bm_cache);
        }

        if matches!(event, ActionEvent::Press(Action::Menu)) {
            let active = self.launcher.active();
            let actions: &[_] = with_app!(active, self, |app| app.quick_actions());
            self.quick_menu.show(actions);
            self.launcher.ctx.mark_dirty(self.quick_menu.region());
            return Transition::None;
        }

        let active = self.launcher.active();
        with_app!(active, self, |app| {
            app.on_event(event, &mut self.launcher.ctx)
        })
    }

    fn handle_quick_menu(
        &mut self,
        event: ActionEvent,
        bm_cache: &mut BookmarkCache,
    ) -> Transition {
        let action = match event {
            ActionEvent::Press(a) | ActionEvent::Repeat(a) => a,
            _ => return Transition::None,
        };

        let result = self.quick_menu.on_action(action);

        match result {
            QuickMenuResult::Consumed => {
                if self.quick_menu.dirty {
                    self.launcher.ctx.mark_dirty(self.quick_menu.region());
                    self.quick_menu.dirty = false;
                }
                Transition::None
            }

            QuickMenuResult::Close => {
                let region = self.quick_menu.region();
                self.sync_quick_menu();
                self.launcher.ctx.mark_dirty(region);
                Transition::None
            }

            QuickMenuResult::RefreshScreen => {
                self.sync_quick_menu();
                self.launcher.ctx.request_full_redraw();
                Transition::None
            }

            QuickMenuResult::GoHome => {
                self.sync_quick_menu();
                Transition::Home
            }

            QuickMenuResult::AppTrigger(id) => {
                let active = self.launcher.active();
                let region = self.quick_menu.region();
                self.sync_quick_menu();

                with_app!(active, self, |app| {
                    app.on_quick_trigger(id, &mut self.launcher.ctx);
                    // Save app state after trigger (e.g. font change
                    // may invalidate the reader's current page offset).
                    app.save_state(bm_cache);
                });

                self.launcher.ctx.mark_dirty(region);
                Transition::None
            }
        }
    }

    pub fn apply_transition(&mut self, transition: Transition, k: &mut KernelHandle<'_>) {
        if let Some(nav) = self.launcher.apply(transition) {
            log::info!("app: {:?} -> {:?}", nav.from, nav.to);

            if nav.from != AppId::Upload {
                with_app!(nav.from, self, |app| {
                    app.save_state(k.bookmark_cache_mut());
                    if nav.suspend {
                        app.on_suspend();
                    } else {
                        app.on_exit();
                    }
                });
            }

            self.propagate_fonts();

            if nav.to != AppId::Upload {
                if nav.resume {
                    with_app!(nav.to, self, |app| {
                        app.on_resume(&mut self.launcher.ctx, k)
                    });
                } else {
                    with_app!(nav.to, self, |app| {
                        app.on_enter(&mut self.launcher.ctx, k)
                    });
                }
            }

            if nav.resume {
                self.launcher
                    .ctx
                    .mark_dirty(Region::new(0, 0, SCREEN_W, SCREEN_H));
            } else {
                self.launcher.ctx.request_full_redraw();
            }
        }
    }

    pub async fn run_background(&mut self, k: &mut KernelHandle<'_>) {
        let active = self.launcher.active();
        with_app!(active, self, |app| {
            app.background(&mut self.launcher.ctx, k).await
        });

        for &id in &[AppId::Home, AppId::Files, AppId::Reader, AppId::Settings] {
            if id != active {
                with_app!(id, self, |app| {
                    if app.has_background_when_suspended() {
                        app.background_suspended(k);
                    }
                });
            }
        }
    }

    pub fn draw(&self, strip: &mut StripBuffer) {
        let active = self.launcher.active();
        with_app_ref!(active, self, |app| app.draw(strip));

        if self.quick_menu.open {
            self.quick_menu.draw(strip);
        }

        self.bumps.draw(strip);
    }

    pub fn propagate_fonts(&mut self) {
        let ui_idx = self.settings.system_settings().ui_font_size_idx;
        let book_idx = self.settings.system_settings().book_font_size_idx;

        self.home.set_ui_font_size(ui_idx);
        self.files.set_ui_font_size(ui_idx);
        self.settings.set_ui_font_size(ui_idx);
        self.reader.set_book_font_size(book_idx);

        let chrome = fonts::chrome_font();
        self.reader.set_chrome_font(chrome);
        self.quick_menu.set_chrome_font(chrome);
        self.bumps.set_chrome_font(chrome);
    }

    fn sync_quick_menu(&mut self) {
        let active = self.launcher.active();

        for id in 0..MAX_APP_ACTIONS as u8 {
            if let Some(value) = self.quick_menu.app_cycle_value(id) {
                with_app!(active, self, |app| {
                    app.on_quick_cycle_update(id, value, &mut self.launcher.ctx);
                });
            }
        }

        let pending = with_app!(active, self, |app| app.pending_setting());
        if let Some(setting) = pending {
            match setting {
                PendingSetting::BookFontSize(idx) => {
                    let ss = self.settings.system_settings_mut();
                    if ss.book_font_size_idx != idx {
                        ss.book_font_size_idx = idx;
                        self.settings.mark_save_needed();
                    }
                }
            }
        }
    }

    #[inline]
    pub fn system_settings(&self) -> &crate::kernel::config::SystemSettings {
        self.settings.system_settings()
    }

    #[inline]
    pub fn settings_loaded(&self) -> bool {
        self.settings.is_loaded()
    }

    #[inline]
    pub fn wifi_config(&self) -> &crate::kernel::config::WifiConfig {
        self.settings.wifi_config()
    }

    pub fn ghost_clear_every(&self) -> u32 {
        if self.settings.is_loaded() {
            self.settings.system_settings().ghost_clear_every as u32
        } else {
            crate::kernel::DEFAULT_GHOST_CLEAR_EVERY
        }
    }
}

impl AppLayer for AppManager {
    type Id = AppId;

    #[inline]
    fn active(&self) -> AppId {
        self.launcher.active()
    }

    fn dispatch_event(&mut self, event: Event, bm: &mut BookmarkCache) -> Transition {
        AppManager::dispatch_event(self, event, bm)
    }

    fn apply_transition(&mut self, t: Transition, k: &mut KernelHandle<'_>) {
        AppManager::apply_transition(self, t, k);
    }

    async fn run_background(&mut self, k: &mut KernelHandle<'_>) {
        AppManager::run_background(self, k).await;
    }

    fn draw(&self, strip: &mut StripBuffer) {
        AppManager::draw(self, strip);
    }

    #[inline]
    fn has_redraw(&self) -> bool {
        self.launcher.ctx.has_redraw()
    }

    #[inline]
    fn take_redraw(&mut self) -> Redraw {
        self.launcher.ctx.take_redraw()
    }

    #[inline]
    fn request_full_redraw(&mut self) {
        self.launcher.ctx.request_full_redraw();
    }

    #[inline]
    fn ctx_mut(&mut self) -> &mut AppContext {
        &mut self.launcher.ctx
    }

    fn system_settings(&self) -> &SystemSettings {
        self.settings.system_settings()
    }

    fn settings_loaded(&self) -> bool {
        self.settings.is_loaded()
    }

    fn ghost_clear_every(&self) -> u32 {
        AppManager::ghost_clear_every(self)
    }

    fn wifi_config(&self) -> &WifiConfig {
        self.settings.wifi_config()
    }

    fn load_eager_settings(&mut self, k: &mut KernelHandle<'_>) {
        AppManager::load_eager_settings(self, k);
    }

    fn load_initial_state(&mut self, k: &mut KernelHandle<'_>) {
        AppManager::load_home_recent(self, k);
    }

    fn enter_initial(&mut self, k: &mut KernelHandle<'_>) {
        AppManager::enter_initial(self, k);
    }

    fn needs_special_mode(&self) -> bool {
        self.launcher.active() == AppId::Upload
    }

    async fn run_special_mode(
        &mut self,
        epd: &mut Epd,
        strip: &mut StripBuffer,
        delay: &mut Delay,
        sd: &SdStorage,
    ) {
        // Safety: WIFI is not owned by any other driver.  Upload mode
        // runs in isolation (the scheduler exits the main dispatch loop
        // first) and tears down the radio stack before returning.  The
        // peripheral is not accessed again until the next upload session.
        let wifi = unsafe { esp_hal::peripherals::WIFI::steal() };

        crate::apps::upload::run_upload_mode(
            wifi,
            epd,
            strip,
            delay,
            sd,
            self.settings.system_settings().ui_font_size_idx,
            &*self.bumps,
            self.settings.wifi_config(),
        )
        .await;
    }

    fn suppress_deferred_input(&self) -> bool {
        self.quick_menu.open
    }
}
