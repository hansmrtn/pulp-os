// Application framework and launcher
//
// Stack-allocated app structs behind the App trait. Launcher holds a
// nav stack (max 4) and AppContext for messaging/redraw. No dyn, no heap.
// Apps needing SD return needs_work()=true; kernel calls on_work() with
// a Services handle before rendering. Services is the syscall boundary.

pub mod bookmarks;
pub mod files;
pub mod home;
pub mod reader;
pub mod settings;

use crate::board::action::ActionEvent;
use crate::drivers::sdcard::SdStorage;
use crate::drivers::storage::{self, DirCache, DirEntry, DirPage};
use crate::drivers::strip::StripBuffer;
use crate::ui::Region;
use crate::ui::quick_menu::QuickAction;

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

    // full GC refresh; use mark_dirty for small same-screen updates
    pub fn request_screen_redraw(&mut self) {
        self.request_full_redraw();
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

// Each call re-opens volume/dir/file; reader uses prefetch to amortize cost.
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

    pub fn write_file(&self, name: &str, data: &[u8]) -> Result<(), &'static str> {
        storage::write_file(self.sd, name, data)
    }

    // ── Subdirectory operations (EPUB chapter cache) ──────────────

    pub fn ensure_dir(&self, name: &str) -> Result<(), &'static str> {
        storage::ensure_dir(self.sd, name)
    }

    pub fn write_in_dir(&self, dir: &str, name: &str, data: &[u8]) -> Result<(), &'static str> {
        storage::write_file_in_dir(self.sd, dir, name, data)
    }

    pub fn append_in_dir(&self, dir: &str, name: &str, data: &[u8]) -> Result<(), &'static str> {
        storage::append_file_in_dir(self.sd, dir, name, data)
    }

    pub fn read_chunk_in_dir(
        &self,
        dir: &str,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        storage::read_file_chunk_in_dir(self.sd, dir, name, offset, buf)
    }

    pub fn file_size_in_dir(&self, dir: &str, name: &str) -> Result<u32, &'static str> {
        storage::file_size_in_dir(self.sd, dir, name)
    }

    pub fn delete_in_dir(&self, dir: &str, name: &str) -> Result<(), &'static str> {
        storage::delete_file_in_dir(self.sd, dir, name)
    }

    // ── _PULP app-data directory ──────────────────────────────────

    pub fn ensure_pulp_dir(&self) -> Result<(), &'static str> {
        storage::ensure_pulp_dir(self.sd)
    }

    // file ops directly inside _PULP/

    pub fn read_pulp_start(
        &self,
        name: &str,
        buf: &mut [u8],
    ) -> Result<(u32, usize), &'static str> {
        storage::read_pulp_file_start(self.sd, name, buf)
    }

    pub fn read_pulp_chunk(
        &self,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        storage::read_pulp_file_chunk(self.sd, name, offset, buf)
    }

    pub fn write_pulp(&self, name: &str, data: &[u8]) -> Result<(), &'static str> {
        storage::write_pulp_file(self.sd, name, data)
    }

    // nested subdir ops inside _PULP/<dir>/

    pub fn ensure_pulp_subdir(&self, name: &str) -> Result<(), &'static str> {
        storage::ensure_pulp_subdir(self.sd, name)
    }

    pub fn write_pulp_sub(&self, dir: &str, name: &str, data: &[u8]) -> Result<(), &'static str> {
        storage::write_in_pulp_subdir(self.sd, dir, name, data)
    }

    pub fn append_pulp_sub(&self, dir: &str, name: &str, data: &[u8]) -> Result<(), &'static str> {
        storage::append_in_pulp_subdir(self.sd, dir, name, data)
    }

    pub fn read_pulp_sub_chunk(
        &self,
        dir: &str,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        storage::read_chunk_in_pulp_subdir(self.sd, dir, name, offset, buf)
    }

    pub fn file_size_pulp_sub(&self, dir: &str, name: &str) -> Result<u32, &'static str> {
        storage::file_size_in_pulp_subdir(self.sd, dir, name)
    }

    pub fn delete_pulp_sub(&self, dir: &str, name: &str) -> Result<(), &'static str> {
        storage::delete_in_pulp_subdir(self.sd, dir, name)
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
    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition;

    // help text for current state
    fn help_text(&self) -> &'static str {
        ""
    }

    // app items for quick menu; empty = core only
    fn quick_actions(&self) -> &[QuickAction] {
        &[]
    }

    // quick menu trigger fired; id from QuickAction
    fn on_quick_trigger(&mut self, _id: u8, _ctx: &mut AppContext) {}

    // quick menu cycle value updated
    fn on_quick_cycle_update(&mut self, _id: u8, _value: u8, _ctx: &mut AppContext) {}

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
