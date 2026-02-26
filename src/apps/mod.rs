// Application framework and launcher
//
// Apps are stack allocated structs behind the App trait. The Launcher
// holds a fixed depth navigation stack (max 4) and an AppContext for
// inter-app messaging and redraw requests. No dyn dispatch, no heap.
//
// Lifecycle: on_enter -> on_event* -> on_suspend/on_exit
//            on_resume -> on_event* -> on_exit
//
// Async I/O: apps that need SD access return needs_work() = true.
// The kernel calls on_work() with a Services handle before rendering.
// This prevents stale renders (the "render ownership invariant"):
// if needs_work() is true, PollInput will not enqueue Render.
//
// Services is the syscall boundary. Apps never touch SPI or caches
// directly. Generic over SPI so board types do not leak in.

pub mod files;
pub mod home;
pub mod reader;
pub mod settings;

use crate::board::SdStorage;
use crate::board::strip::StripBuffer;
use crate::drivers::input::Event;
use crate::drivers::storage::{self, DirCache, DirEntry, DirPage};
use crate::ui::{Region, SCREEN_REGION};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppId {
    Home,
    Files,
    Reader,
    Settings,
}

// Push: new app on top, old app suspended (gets on_resume later)
// Pop:  return to parent, current app exits
// Replace: swap in place, no back navigation
// Home: unwind entire stack back to Home
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

// 64 bytes is enough for an 8.3 filename or short path
const MSG_BUF_SIZE: usize = 64;

pub struct AppContext {
    msg_buf: [u8; MSG_BUF_SIZE],
    msg_len: usize,
    redraw: Redraw,
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

    /// Repaint the entire screen using a partial-waveform refresh.
    /// The kernel may promote this to a full hardware refresh
    /// periodically to clear ghosting artifacts.
    pub fn request_screen_redraw(&mut self) {
        self.request_partial_redraw(SCREEN_REGION);
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

    // primary way apps request visual updates; coalesces via bounding box
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

// Each read_file_chunk / file_size call re-opens volume/dir/file.
// The reader mitigates this with lazy indexing and prefetch so the
// hot path (forward page turn) needs at most one SD open per turn.
// read_file_start folds file_size + first read into a single open.
pub struct Services<'a, SPI: embedded_hal::spi::SpiDevice> {
    dir_cache: &'a mut DirCache,
    sd: &'a SdStorage<SPI>,
}

impl<'a, SPI: embedded_hal::spi::SpiDevice> Services<'a, SPI> {
    pub fn new(dir_cache: &'a mut DirCache, sd: &'a SdStorage<SPI>) -> Self {
        Self { dir_cache, sd }
    }

    pub fn dir_page(
        &mut self,
        offset: usize,
        buf: &mut [DirEntry],
    ) -> Result<DirPage, &'static str> {
        self.dir_cache.ensure_loaded(self.sd)?;
        Ok(self.dir_cache.page(offset, buf))
    }

    pub fn invalidate_dir_cache(&mut self) {
        self.dir_cache.invalidate();
    }

    pub fn read_file_chunk(
        &self,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        storage::read_file_chunk(self.sd, name, offset, buf)
    }

    // open file once, return (file_size, bytes_read) from offset 0
    pub fn read_file_start(
        &self,
        name: &str,
        buf: &mut [u8],
    ) -> Result<(u32, usize), &'static str> {
        storage::read_file_start(self.sd, name, buf)
    }

    pub fn file_size(&self, name: &str) -> Result<u32, &'static str> {
        storage::file_size(self.sd, name)
    }
}

pub trait App {
    fn on_enter(&mut self, ctx: &mut AppContext);
    fn on_exit(&mut self) {}
    fn on_suspend(&mut self) {
        self.on_exit();
    }
    fn on_resume(&mut self, ctx: &mut AppContext) {
        self.on_enter(ctx);
    }
    fn on_event(&mut self, event: Event, ctx: &mut AppContext) -> Transition;

    // called once per strip during refresh; widgets clip automatically
    fn draw(&self, strip: &mut StripBuffer);

    fn needs_work(&self) -> bool {
        false
    }
    fn on_work<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        _services: &mut Services<'_, SPI>,
        _ctx: &mut AppContext,
    ) {
    }
}

const MAX_STACK_DEPTH: usize = 4;

#[derive(Debug, Clone, Copy)]
pub struct NavEvent {
    pub from: AppId,
    pub to: AppId,
    pub suspend: bool,
    pub resume: bool,
}

pub struct Launcher {
    stack: [AppId; MAX_STACK_DEPTH],
    depth: usize,
    pub ctx: AppContext,
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
