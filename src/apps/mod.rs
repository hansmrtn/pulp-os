// app trait, nav stack, and app lifecycle

pub mod files;
pub mod home;
pub mod manager;
pub mod reader;

pub mod settings;
pub mod upload;

use crate::board::action::ActionEvent;
#[allow(unused_imports)]
use crate::drivers::strip::StripBuffer;
use crate::kernel::KernelHandle;
use crate::kernel::bookmarks::BookmarkCache;
use crate::ui::Region;
use crate::ui::quick_menu::QuickAction;

// cross-app constants
pub const RECENT_FILE: &str = "RECENT";

#[derive(Clone, Copy, Debug)]
pub enum PendingSetting {
    BookFontSize(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppId {
    Home,
    Files,
    Reader,
    Settings,
    // upload bypasses the App trait; scheduler intercepts this
    // variant and calls run_upload_mode directly
    Upload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    None,
    Push(AppId),
    Pop,
    Replace(AppId),
    Home,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redraw {
    None,
    Partial(Region),
    Full,
}

const MSG_BUF_SIZE: usize = 64;

pub struct AppContext {
    msg_buf: [u8; MSG_BUF_SIZE],
    msg_len: usize,
    redraw: Redraw,
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

    #[inline]
    pub fn mark_dirty(&mut self, region: Region) {
        self.request_partial_redraw(region);
    }

    pub fn has_redraw(&self) -> bool {
        !matches!(self.redraw, Redraw::None)
    }

    pub fn take_redraw(&mut self) -> Redraw {
        let r = self.redraw;
        self.redraw = Redraw::None;
        r
    }
}

#[allow(async_fn_in_trait)]
pub trait App {
    async fn on_enter(&mut self, ctx: &mut AppContext, k: &mut KernelHandle<'_>);

    fn on_exit(&mut self) {}

    fn on_suspend(&mut self) {
        self.on_exit();
    }

    async fn on_resume(&mut self, ctx: &mut AppContext, k: &mut KernelHandle<'_>) {
        self.on_enter(ctx, k).await;
    }

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition;

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
pub struct NavEvent {
    pub from: AppId,
    pub to: AppId,
    pub suspend: bool,
    pub resume: bool,
}

// 4-deep navigation stack with shared AppContext
pub struct Launcher {
    stack: [AppId; MAX_STACK_DEPTH],
    depth: usize,
    pub ctx: AppContext,
}

impl Default for Launcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Launcher {
    pub const fn new() -> Self {
        Self {
            stack: [AppId::Home; MAX_STACK_DEPTH],
            depth: 1,
            ctx: AppContext::new(),
        }
    }

    pub fn active(&self) -> AppId {
        self.stack[self.depth - 1]
    }

    pub fn apply(&mut self, transition: Transition) -> Option<NavEvent> {
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
                self.stack[0] = AppId::Home;
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
