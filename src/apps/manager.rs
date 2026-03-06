// app lifecycle manager: nav stack, dispatch, font propagation, draw
//
// all dispatch is static (monomorphized via with_app!); no dyn, no vtable
// loading indicator is drawn between app content and overlays so it
// sits on top of page content but under quick menu and button bumps

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

// monomorphized dispatch from AppId to concrete app type;
// two arms: `mut` for &mut *, `ref` for &*
macro_rules! dispatch_app {
    ($id:expr, $mgr:expr, mut, |$app:ident| $body:expr) => {
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
                unreachable!("Upload handled outside dispatch");
            }
        }
    };
    ($id:expr, $mgr:expr, ref, |$app:ident| $body:expr) => {
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
                unreachable!("Upload handled outside dispatch");
            }
        }
    };
}

macro_rules! with_app {
    ($id:expr, $mgr:expr, |$app:ident| $body:expr) => {
        dispatch_app!($id, $mgr, mut, |$app| $body)
    };
}

macro_rules! with_app_ref {
    ($id:expr, $mgr:expr, |$app:ident| $body:expr) => {
        dispatch_app!($id, $mgr, ref, |$app| $body)
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
        self.sync_button_config();
    }

    // sync button mapper and label widget from settings
    pub fn sync_button_config(&mut self) {
        let swap = self.settings.system_settings().swap_buttons;
        self.mapper.set_swap(swap);
        if self.bumps.set_swap(swap) {
            // labels changed, need to redraw the button bar
            self.launcher.ctx.mark_dirty(crate::ui::Region::new(
                0,
                crate::board::SCREEN_H - crate::ui::BUTTON_BAR_H,
                crate::board::SCREEN_W,
                crate::ui::BUTTON_BAR_H,
            ));
        }
    }

    pub fn load_home_recent(&mut self, k: &mut KernelHandle<'_>) {
        self.home.load_recent(k);
    }

    pub fn enter_initial(&mut self, k: &mut KernelHandle<'_>) {
        self.home.on_enter(&mut self.launcher.ctx, k);
    }

    // collect session state to RTC memory struct before sleep
    pub fn collect_session(&self, session: &mut crate::kernel::rtc_session::RtcSession) {
        use crate::kernel::rtc_session::MAX_NAV_STACK;

        // save navigation stack
        session.nav_depth = self.launcher.depth() as u8;
        for i in 0..MAX_NAV_STACK {
            session.nav_stack[i] = if i < self.launcher.depth() {
                self.launcher.stack_at(i) as u8
            } else {
                0
            };
        }

        // save reader state
        session.reader_filename_len = self.reader.filename_len() as u8;
        let len = session.reader_filename_len as usize;
        session.reader_filename[..len].copy_from_slice(self.reader.filename_bytes());
        session.reader_is_epub = self.reader.is_epub() as u8;
        session.reader_chapter = self.reader.chapter();
        session.reader_page = self.reader.page() as u16;
        session.reader_byte_offset = self.reader.byte_offset();
        session.reader_font_size = self.reader.font_size_idx();

        // save files state
        session.files_scroll = self.files.scroll() as u16;
        session.files_selected = self.files.selected() as u8;
        session.files_total = self.files.total() as u16;

        // save home state
        session.home_state = self.home.state_id();
        session.home_selected = self.home.selected() as u8;
        session.home_bm_selected = self.home.bm_selected() as u8;
        session.home_bm_scroll = self.home.bm_scroll() as u8;

        // save settings cache
        let ss = self.settings.system_settings();
        session.settings_sleep_timeout = ss.sleep_timeout;
        session.settings_ghost_clear = ss.ghost_clear_every;
        session.settings_book_font = ss.book_font_size_idx;
        session.settings_ui_font = ss.ui_font_size_idx;
        session.settings_valid = 1;

        log::info!(
            "session: collected nav_depth={} active={:?}",
            session.nav_depth,
            self.launcher.active()
        );
    }

    // restore session from RTC memory; returns true if successful
    pub fn apply_session(
        &mut self,
        session: &crate::kernel::rtc_session::RtcSession,
        k: &mut KernelHandle<'_>,
    ) -> bool {
        // validate session data
        if session.nav_depth == 0 || session.nav_depth > 4 {
            log::warn!("session: invalid nav_depth {}", session.nav_depth);
            return false;
        }

        // restore navigation stack
        self.launcher.restore_stack(
            session.nav_depth as usize,
            &session.nav_stack,
            |id| match id {
                0 => AppId::Home,
                1 => AppId::Files,
                2 => AppId::Reader,
                3 => AppId::Settings,
                _ => AppId::Home,
            },
        );

        log::info!(
            "session: restored nav stack depth={} active={:?}",
            session.nav_depth,
            self.launcher.active()
        );

        // restore home state (always in stack)
        self.home.restore_state(
            session.home_state,
            session.home_selected as usize,
            session.home_bm_selected as usize,
            session.home_bm_scroll as usize,
        );

        // restore files state if in stack
        if self.launcher.contains(AppId::Files) {
            self.files.restore_state(
                session.files_scroll as usize,
                session.files_selected as usize,
                session.files_total as usize,
            );
        }

        // restore reader state if active or in stack
        if self.launcher.active() == AppId::Reader || self.launcher.contains(AppId::Reader) {
            let filename = &session.reader_filename[..session.reader_filename_len as usize];
            self.reader.restore_state(
                filename,
                session.reader_is_epub != 0,
                session.reader_chapter,
                session.reader_page as usize,
                session.reader_byte_offset,
                session.reader_font_size,
            );
        }

        // propagate fonts before entering apps
        self.propagate_fonts();

        // enter apps in stack order (bottom to top)
        // Home is always at bottom
        self.home.on_enter(&mut self.launcher.ctx, k);

        // for apps above Home, call on_suspend for suspended ones, on_enter for active
        let depth = self.launcher.depth();
        for i in 1..depth {
            let app_id = self.launcher.stack_at(i);
            let is_active = i == depth - 1;

            match app_id {
                AppId::Files => {
                    if is_active {
                        self.files.on_enter(&mut self.launcher.ctx, k);
                    } else {
                        // Files was pushed then another app pushed on top
                        self.files.on_enter(&mut self.launcher.ctx, k);
                        self.files.on_suspend();
                    }
                }
                AppId::Reader => {
                    if is_active {
                        // set message for reader to know filename
                        let filename =
                            &session.reader_filename[..session.reader_filename_len as usize];
                        self.launcher.ctx.set_message(filename);
                        self.reader.on_enter(&mut self.launcher.ctx, k);
                    }
                }
                AppId::Settings => {
                    if is_active {
                        self.settings.on_enter(&mut self.launcher.ctx, k);
                    }
                }
                _ => {}
            }

            // suspend apps that aren't the top
            if !is_active {
                match app_id {
                    AppId::Home => self.home.on_suspend(),
                    AppId::Files => {} // already handled above
                    _ => {}
                }
            }
        }

        // mark full redraw needed
        self.launcher.ctx.request_full_redraw();

        true
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
            self.launcher.ctx.clear_loading();

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

        // sync button configuration from settings (may have changed)
        self.sync_button_config();
    }

    pub fn draw(&self, strip: &mut StripBuffer) {
        let active = self.launcher.active();
        with_app_ref!(active, self, |app| app.draw(strip));

        // loading indicator: after app content, before overlays
        if self.launcher.ctx.loading_active() {
            let region = self.launcher.ctx.loading_region();
            if region.intersects(strip.logical_window()) {
                crate::ui::draw_loading_indicator(
                    strip,
                    region,
                    self.launcher.ctx.loading_msg(),
                    self.launcher.ctx.loading_pct(),
                );
            }
        }

        if self.quick_menu.open {
            self.quick_menu.draw(strip);
        }

        self.bumps.draw(strip);
    }

    pub fn propagate_fonts(&mut self) {
        let ss = self.settings.system_settings();
        let ui_idx = ss.ui_font_size_idx;
        let book_idx = ss.book_font_size_idx;
        let theme_idx = ss.reading_theme;

        self.home.set_ui_font_size(ui_idx);
        self.files.set_ui_font_size(ui_idx);
        self.settings.set_ui_font_size(ui_idx);
        self.reader.set_book_font_size(book_idx);
        self.reader.set_reading_theme(theme_idx);

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

    fn collect_session(&self, session: &mut crate::kernel::rtc_session::RtcSession) {
        AppManager::collect_session(self, session);
    }

    fn apply_session(
        &mut self,
        session: &crate::kernel::rtc_session::RtcSession,
        k: &mut KernelHandle<'_>,
    ) -> bool {
        AppManager::apply_session(self, session, k)
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
