// app protocol: trait, context, transitions, redraw types, coalescing,
// and loading indicator state
//
// these types define the contract between the kernel scheduler and
// the app layer. concrete apps implement the App trait; the kernel
// drives lifecycle, input dispatch, and rendering through it.
//
// the kernel is generic over an app identity type (AppIdType).
// distros define their own AppId enum and implement AppIdType for
// it. the kernel never knows which specific apps exist.
//
// QuickAction types also live here -- they are pure data describing
// what actions an app exposes; the renderer (QuickMenu widget) is
// app-side, but the protocol is kernel-side

use embassy_time::Instant;
use esp_hal::delay::Delay;

use crate::board::Epd;
use crate::board::action::ActionEvent;
use crate::drivers::input::Event;
use crate::drivers::sdcard::SdStorage;
#[allow(unused_imports)]
use crate::drivers::strip::StripBuffer;
use crate::ui::Region;

use super::KernelHandle;
use super::bookmarks::BookmarkCache;
use super::config::{SystemSettings, WifiConfig};

pub const MAX_APP_ACTIONS: usize = 6;

#[derive(Debug, Clone, Copy)]
pub enum QuickActionKind {
    Cycle {
        value: u8,
        options: &'static [&'static str],
    },
    Trigger {
        display: &'static str,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct QuickAction {
    pub id: u8,
    pub label: &'static str,
    pub kind: QuickActionKind,
}

impl QuickAction {
    pub const fn cycle(
        id: u8,
        label: &'static str,
        value: u8,
        options: &'static [&'static str],
    ) -> Self {
        Self {
            id,
            label,
            kind: QuickActionKind::Cycle { value, options },
        }
    }

    pub const fn trigger(id: u8, label: &'static str, display: &'static str) -> Self {
        Self {
            id,
            label,
            kind: QuickActionKind::Trigger { display },
        }
    }
}

pub const RECENT_FILE: &str = "RECENT";

// distros define their own AppId enum and implement this trait
// the kernel uses HOME to initialise the nav stack and reset on
// Transition::Home; nothing else about the concrete variants is
// known to the kernel

pub trait AppIdType: Copy + Eq + core::fmt::Debug {
    const HOME: Self;
}

#[derive(Clone, Copy, Debug)]
pub enum PendingSetting {
    BookFontSize(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition<Id> {
    None,
    Push(Id),
    Pop,
    Replace(Id),
    Home,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redraw {
    None,
    Partial(Region),
    Full,
}

const MSG_BUF_SIZE: usize = 64;
const LOADING_BUF_SIZE: usize = 32;

pub struct AppContext {
    msg_buf: [u8; MSG_BUF_SIZE],
    msg_len: usize,
    redraw: Redraw,
    coalesce_until: Option<Instant>,
    immediate: bool,

    // loading indicator; kernel-level so any app can use it.
    // drawn by the app manager after app content, before overlays.
    // uses the built-in mono font so it works even with no bitmap
    // fonts loaded.
    loading_buf: [u8; LOADING_BUF_SIZE],
    loading_len: u8,
    loading_pct: u8,
    loading_active: bool,
    loading_region: Region,
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new()
    }
}

impl AppContext {
    pub const fn new() -> Self {
        Self {
            msg_buf: [0u8; MSG_BUF_SIZE],
            msg_len: 0,
            redraw: Redraw::None,
            coalesce_until: None,
            immediate: false,
            loading_buf: [0u8; LOADING_BUF_SIZE],
            loading_len: 0,
            loading_pct: 0,
            loading_active: false,
            loading_region: Region::new(0, 0, 0, 0),
        }
    }

    pub fn set_message(&mut self, data: &[u8]) {
        let len = data.len().min(MSG_BUF_SIZE);
        self.msg_buf[..len].copy_from_slice(&data[..len]);
        self.msg_len = len;
    }

    pub fn message(&self) -> &[u8] {
        &self.msg_buf[..self.msg_len]
    }

    pub fn message_str(&self) -> &str {
        core::str::from_utf8(self.message()).unwrap_or("")
    }

    pub fn clear_message(&mut self) {
        self.msg_len = 0;
    }

    pub fn request_full_redraw(&mut self) {
        self.redraw = Redraw::Full;
    }

    pub fn request_partial_redraw(&mut self, region: Region) {
        match self.redraw {
            Redraw::Full => {}
            Redraw::Partial(existing) => {
                self.redraw = Redraw::Partial(existing.union(region));
            }
            Redraw::None => self.redraw = Redraw::Partial(region),
        }
    }

    // mark dirty and render on next tick; the default for all callers
    #[inline]
    pub fn mark_dirty(&mut self, region: Region) {
        self.request_partial_redraw(region);
        self.immediate = true;
        self.coalesce_until = None;
    }

    // mark dirty with coalescing window; use only for background
    // batch updates (title scanner) where many rapid dirty marks
    // should coalesce into a single refresh
    #[inline]
    pub fn mark_dirty_coalesced(&mut self, region: Region) {
        use super::timing;
        self.request_partial_redraw(region);
        if !self.immediate && self.coalesce_until.is_none() {
            self.coalesce_until =
                Some(Instant::now() + embassy_time::Duration::from_millis(timing::COALESCE_WINDOW_MS));
        }
    }

    pub fn has_redraw(&self) -> bool {
        !matches!(self.redraw, Redraw::None)
    }

    // true when a pending redraw is ready to render
    pub fn render_ready(&self) -> bool {
        match self.redraw {
            Redraw::None => false,
            Redraw::Full => true,
            Redraw::Partial(_) => {
                self.immediate || self.coalesce_until.map_or(true, |t| Instant::now() >= t)
            }
        }
    }

    pub fn take_redraw(&mut self) -> Redraw {
        let r = self.redraw;
        self.redraw = Redraw::None;
        self.coalesce_until = None;
        self.immediate = false;
        r
    }

    // loading indicator: set text and percentage.
    // draws "msg...pct%" using the built-in mono font.
    // region defines where it renders; typically just below the
    // app header in the content area.
    // auto-marks the region dirty so the next render shows it.
    pub fn set_loading(&mut self, region: Region, msg: &str, pct: u8) {
        let n = msg.len().min(LOADING_BUF_SIZE);
        self.loading_buf[..n].copy_from_slice(&msg[..n].as_bytes()[..n]);
        self.loading_len = n as u8;
        self.loading_pct = pct.min(100);
        self.loading_region = region;

        self.loading_active = true;
        self.mark_dirty(region);
    }

    // clear the loading indicator and mark its region dirty so
    // the underlying content repaints
    pub fn clear_loading(&mut self) {
        if self.loading_active {
            let region = self.loading_region;
            self.loading_active = false;
            self.loading_len = 0;
            self.loading_pct = 0;
            self.mark_dirty(region);
        }
    }

    #[inline]
    pub fn loading_active(&self) -> bool {
        self.loading_active
    }

    #[inline]
    pub fn loading_msg(&self) -> &str {
        core::str::from_utf8(&self.loading_buf[..self.loading_len as usize]).unwrap_or("")
    }

    #[inline]
    pub fn loading_pct(&self) -> u8 {
        self.loading_pct
    }

    #[inline]
    pub fn loading_region(&self) -> Region {
        self.loading_region
    }
}

// background is async for epub streaming (stream_strip_entry_async);
// other app impls compile to immediately-ready futures
#[allow(async_fn_in_trait)]
pub trait App<Id> {
    fn on_enter(&mut self, ctx: &mut AppContext, k: &mut KernelHandle<'_>);

    fn on_exit(&mut self) {}

    fn on_suspend(&mut self) {
        self.on_exit();
    }

    fn on_resume(&mut self, ctx: &mut AppContext, k: &mut KernelHandle<'_>) {
        self.on_enter(ctx, k);
    }

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition<Id>;

    fn quick_actions(&self) -> &[QuickAction] {
        &[]
    }

    fn on_quick_trigger(&mut self, _id: u8, _ctx: &mut AppContext) {}

    fn on_quick_cycle_update(&mut self, _id: u8, _value: u8, _ctx: &mut AppContext) {}

    fn draw(&self, strip: &mut StripBuffer);

    async fn background(&mut self, _ctx: &mut AppContext, _k: &mut KernelHandle<'_>) {}

    fn pending_setting(&self) -> Option<PendingSetting> {
        None
    }

    fn save_state(&self, _bm: &mut BookmarkCache) {}

    fn has_background_when_suspended(&self) -> bool {
        false
    }

    fn background_suspended(&mut self, _k: &mut KernelHandle<'_>) {}
}

const MAX_STACK_DEPTH: usize = 4;

#[derive(Debug, Clone, Copy)]
pub struct NavEvent<Id> {
    pub from: Id,
    pub to: Id,
    pub suspend: bool,
    pub resume: bool,
}

// 4-deep navigation stack with shared AppContext
pub struct Launcher<Id: AppIdType> {
    stack: [Id; MAX_STACK_DEPTH],
    depth: usize,
    pub ctx: AppContext,
}

impl<Id: AppIdType> Default for Launcher<Id> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Id: AppIdType> Launcher<Id> {
    pub const fn new() -> Self {
        Self {
            stack: [Id::HOME; MAX_STACK_DEPTH],
            depth: 1,
            ctx: AppContext::new(),
        }
    }

    #[inline]
    pub fn active(&self) -> Id {
        self.stack[self.depth - 1]
    }

    #[inline]
    pub fn depth(&self) -> usize {
        self.depth
    }

    #[inline]
    pub fn stack_at(&self, index: usize) -> Id {
        self.stack[index]
    }

    /// Check if an app ID is anywhere in the stack
    pub fn contains(&self, id: Id) -> bool {
        self.stack[..self.depth].iter().any(|&i| i == id)
    }

    /// Restore stack from saved session data
    ///
    /// The `convert` function maps u8 values to Id.
    pub fn restore_stack<F>(&mut self, depth: usize, stack: &[u8], convert: F)
    where
        F: Fn(u8) -> Id,
    {
        self.depth = depth.min(MAX_STACK_DEPTH).max(1);
        for i in 0..self.depth {
            self.stack[i] = convert(stack[i]);
        }
    }

    pub fn apply(&mut self, transition: Transition<Id>) -> Option<NavEvent<Id>> {
        let old = self.active();

        let (suspend, resume) = match transition {
            Transition::None => return None,

            Transition::Push(id) => {
                if self.depth >= MAX_STACK_DEPTH {
                    log::warn!(
                        "nav stack full (depth {}), Push({:?}) degraded to Replace",
                        self.depth,
                        id
                    );
                    self.stack[self.depth - 1] = id;
                    (false, false)
                } else {
                    self.stack[self.depth] = id;
                    self.depth += 1;
                    (true, false)
                }
            }

            Transition::Pop => {
                if self.depth > 1 {
                    self.depth -= 1;
                    (false, true)
                } else {
                    return None;
                }
            }

            Transition::Replace(id) => {
                self.stack[self.depth - 1] = id;
                (false, false)
            }

            Transition::Home => {
                self.depth = 1;
                self.stack[0] = Id::HOME;
                (false, true)
            }
        };

        let new = self.active();
        if new != old {
            Some(NavEvent {
                from: old,
                to: new,
                suspend,
                resume,
            })
        } else {
            None
        }
    }
}

// aggregate interface the kernel scheduler calls on the app layer
// a distro implements this (typically via an AppManager struct that
// holds concrete app types and a with_app! dispatch macro); the
// scheduler is generic over AppLayer without importing any concrete
// app types

// run_special_mode is genuinely async (wifi radio); the rest is sync
#[allow(async_fn_in_trait)]
pub trait AppLayer {
    type Id: AppIdType;

    // active app and event dispatch
    fn active(&self) -> Self::Id;
    fn dispatch_event(&mut self, event: Event, bm: &mut BookmarkCache) -> Transition<Self::Id>;
    fn apply_transition(&mut self, t: Transition<Self::Id>, k: &mut KernelHandle<'_>);

    // background work (SD I/O, caching); async for epub streaming
    async fn run_background(&mut self, k: &mut KernelHandle<'_>);

    // rendering
    fn draw(&self, strip: &mut StripBuffer);
    fn has_redraw(&self) -> bool;
    fn take_redraw(&mut self) -> Redraw;
    fn request_full_redraw(&mut self);
    fn ctx_mut(&mut self) -> &mut AppContext;

    // system configuration
    fn system_settings(&self) -> &SystemSettings;
    fn settings_loaded(&self) -> bool;
    fn ghost_clear_every(&self) -> u32;
    fn wifi_config(&self) -> &WifiConfig;

    // boot-time init: load settings, populate caches, enter first app
    fn load_eager_settings(&mut self, k: &mut KernelHandle<'_>);
    fn load_initial_state(&mut self, k: &mut KernelHandle<'_>);
    fn enter_initial(&mut self, k: &mut KernelHandle<'_>);

    // session persistence: save/restore active app across sleep/wake
    // using RTC FAST memory (survives deep sleep, zeroed on power-on)
    //
    // collect_session writes app state to the provided RtcSession struct
    // apply_session restores app state from RtcSession, returns true if successful
    fn collect_session(&self, session: &mut super::rtc_session::RtcSession);
    fn apply_session(&mut self, session: &super::rtc_session::RtcSession, k: &mut KernelHandle<'_>) -> bool;

    // true when the active app wants to take over the main loop
    // (e.g. wifi upload mode bypasses the normal event dispatch)
    fn needs_special_mode(&self) -> bool {
        false
    }

    // run the special mode; scheduler calls this when
    // needs_special_mode() returns true. hardware resources are
    // passed from the kernel since special modes drive the EPD
    // and SD directly (e.g. wifi upload mode).
    async fn run_special_mode(
        &mut self,
        _epd: &mut Epd,
        _strip: &mut StripBuffer,
        _delay: &mut Delay,
        _sd: &SdStorage,
    ) {
    }

    // true when deferred input during EPD refresh should be
    // suppressed (e.g. quick menu overlay is open)
    fn suppress_deferred_input(&self) -> bool {
        false
    }
}
