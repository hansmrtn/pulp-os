//! App framework for pulp-os
//!
//! Apps are self-contained screen-owning modules. The launcher manages
//! transitions between them and routes input events.
//!
//! # Writing an app
//!
//! ```ignore
//! pub struct MyApp { /* state */ }
//!
//! impl App for MyApp {
//!     fn on_enter(&mut self, ctx: &mut AppContext) {
//!         ctx.request_full_redraw();
//!     }
//!
//!     fn on_event(&mut self, event: Event, ctx: &mut AppContext) -> Transition {
//!         match event {
//!             Event::Press(Button::Back) => Transition::Pop,
//!             Event::Press(Button::Right) => {
//!                 self.selected += 1;
//!                 ctx.mark_dirty(old_region);
//!                 ctx.mark_dirty(new_region);
//!                 Transition::None
//!             }
//!             _ => Transition::None,
//!         }
//!     }
//!
//!     fn draw(&self, strip: &mut StripBuffer) {
//!         // Draw widgets — called per-strip during refresh
//!     }
//!
//!     // For apps with async I/O:
//!     fn needs_work(&self) -> bool { self.needs_load }
//!
//!     fn on_work<SPI: embedded_hal::spi::SpiDevice>(
//!         &mut self, svc: &mut Services<'_, SPI>, ctx: &mut AppContext,
//!     ) {
//!         // Use svc.dir_page(), svc.read_file_chunk(), etc.
//!         // Then ctx.mark_dirty() for changed regions.
//!     }
//! }
//! ```

pub mod files;
pub mod home;
pub mod reader;
pub mod settings;

use crate::board::SdStorage;
use crate::board::strip::StripBuffer;
use crate::drivers::input::Event;
use crate::drivers::storage::{self, DirCache, DirEntry, DirPage};
use crate::ui::Region;

/// Identity of each app in the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppId {
    Home,
    Files,
    Reader,
    Settings,
}

/// What should happen after handling an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Stay on current app, no action.
    None,
    /// Push a new app onto the stack (current stays underneath).
    Push(AppId),
    /// Pop current app, return to the one below.
    Pop,
    /// Replace current app entirely (no back navigation).
    Replace(AppId),
    /// Pop all the way back to Home.
    Home,
}

/// Redraw request from an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redraw {
    /// Nothing to draw.
    None,
    /// Partial refresh of a specific region.
    Partial(Region),
    /// Full screen refresh needed (e.g. on first enter).
    Full,
}

/// Small message buffer for passing data between apps.
/// e.g. file browser sets a path, reader reads it on entry.
const MSG_BUF_SIZE: usize = 64;

/// Shared context passed to apps. Gives access to cross-app
/// communication without apps needing to know about each other.
pub struct AppContext {
    /// Message buffer for inter-app data (file path, etc.)
    msg_buf: [u8; MSG_BUF_SIZE],
    msg_len: usize,
    /// Redraw request set by the app
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

    /// Set a message for the next app (e.g. a file path to open).
    pub fn set_message(&mut self, data: &[u8]) {
        let len = data.len().min(MSG_BUF_SIZE);
        self.msg_buf[..len].copy_from_slice(&data[..len]);
        self.msg_len = len;
    }

    /// Read the message left by the previous app.
    pub fn message(&self) -> &[u8] {
        &self.msg_buf[..self.msg_len]
    }

    /// Read message as UTF-8 string.
    pub fn message_str(&self) -> &str {
        core::str::from_utf8(self.message()).unwrap_or("")
    }

    /// Clear the message buffer.
    pub fn clear_message(&mut self) {
        self.msg_len = 0;
    }

    /// Request a full screen redraw.
    pub fn request_full_redraw(&mut self) {
        self.redraw = Redraw::Full;
    }

    /// Request a partial redraw of a region.
    /// If a partial is already pending, coalesces via bounding box union.
    pub fn request_partial_redraw(&mut self, region: Region) {
        match self.redraw {
            Redraw::Full => {}
            Redraw::Partial(existing) => {
                self.redraw = Redraw::Partial(existing.union(region));
            }
            Redraw::None => self.redraw = Redraw::Partial(region),
        }
    }

    /// Mark a region as needing redraw. Apps call this during `on_event`
    /// for each UI element that changed — the framework coalesces
    /// multiple calls via bounding-box union automatically.
    ///
    /// This is the primary way apps request visual updates. Prefer
    /// marking individual changed elements over full-screen redraws
    /// to minimize e-paper flicker and SPI transfer.
    #[inline]
    pub fn mark_dirty(&mut self, region: Region) {
        self.request_partial_redraw(region);
    }

    pub fn has_redraw(&self) -> bool {
        !matches!(self.redraw, Redraw::None)
    }

    /// Take the pending redraw request (resets to None).
    pub fn take_redraw(&mut self) -> Redraw {
        let r = self.redraw;
        self.redraw = Redraw::None;
        r
    }
}

// ── Services ───────────────────────────────────────────────────
//
// The app-OS boundary. Apps request hardware operations through
// Services methods — they never touch SPI, SD, or caches directly.
//
// Constructed by the kernel per-job, borrowing the long-lived
// state from main(). Zero-cost: just assembles two references.
//
// Generic over SPI so no concrete board types leak into the app
// framework. The compiler monomorphizes to the real type via
// with_app! dispatch in main.rs.

/// OS services available to apps during `on_work`.
///
/// This is the "syscall interface". Apps get high-level operations
/// (load a directory page, read a file chunk) without knowing about
/// SPI buses, SD protocols, or caching strategies.
pub struct Services<'a, SPI: embedded_hal::spi::SpiDevice> {
    dir_cache: &'a mut DirCache,
    sd: &'a SdStorage<SPI>,
}

impl<'a, SPI: embedded_hal::spi::SpiDevice> Services<'a, SPI> {
    /// Construct Services. Called by the kernel at the start of AppWork.
    pub fn new(dir_cache: &'a mut DirCache, sd: &'a SdStorage<SPI>) -> Self {
        Self { dir_cache, sd }
    }

    /// Load a page of directory entries from the root directory.
    ///
    /// Uses an internal cache — first call reads from SD (~100ms),
    /// subsequent calls are pure memory copies (instant).
    pub fn dir_page(
        &mut self,
        offset: usize,
        buf: &mut [DirEntry],
    ) -> Result<DirPage, &'static str> {
        self.dir_cache.ensure_loaded(self.sd)?;
        Ok(self.dir_cache.page(offset, buf))
    }

    /// Mark the directory cache as stale. Next `dir_page()` will
    /// re-read from SD. Call this when the SD card contents may
    /// have changed (e.g. fresh app entry).
    pub fn invalidate_dir_cache(&mut self) {
        self.dir_cache.invalidate();
    }

    /// Read a chunk of a file starting at `offset`.
    /// Returns the number of bytes read into `buf`.
    pub fn read_file_chunk(
        &self,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        storage::read_file_chunk(self.sd, name, offset, buf)
    }

    /// Get the size of a file in the root directory.
    pub fn file_size(&self, name: &str) -> Result<u32, &'static str> {
        storage::file_size(self.sd, name)
    }
}

/// Core app trait. Each app implements this.
///
/// Apps are statically allocated and own their UI state.
/// They don't hold references to hardware — that's passed
/// in via the draw callback and main loop.
///
/// ## Lifecycle
///
/// - `on_enter` — first activation, or replacing another app.
///   Call `ctx.request_full_redraw()`.
/// - `on_exit` — permanently leaving (Pop, Replace, Home).
/// - `on_suspend` — being pushed behind a child app. Override to
///   preserve state (default: delegates to `on_exit`).
/// - `on_resume` — returning from a child app. Override to skip
///   expensive reinit (default: delegates to `on_enter`).
///
/// ## Async work
///
/// Some apps need deferred I/O (loading files, reading SD).
/// Override `needs_work()` to return true when work is pending,
/// and `on_work()` to perform it using OS `Services`. The kernel
/// schedules this as a generic `AppWork` job — no app-specific
/// job variants or handlers needed.
pub trait App {
    /// Called when this app becomes the active screen for the first time
    /// (or via Replace). Read `ctx.message()` for data from the launcher.
    fn on_enter(&mut self, ctx: &mut AppContext);

    /// Called when this app is permanently removed from the stack.
    fn on_exit(&mut self) {
        // Default: no-op
    }

    /// Called when a child app is pushed on top.
    /// The app stays on the stack and will get `on_resume` when the
    /// child pops. Override to preserve state; default delegates to `on_exit`.
    fn on_suspend(&mut self) {
        self.on_exit();
    }

    /// Called when returning from a child app (the child popped).
    /// Override to skip reloading data that's still valid.
    /// Default delegates to `on_enter`.
    fn on_resume(&mut self, ctx: &mut AppContext) {
        self.on_enter(ctx);
    }

    /// Handle an input event. Return a transition to navigate.
    ///
    /// Call `ctx.mark_dirty(region)` for each UI element that changed.
    /// The framework coalesces multiple dirty regions automatically.
    fn on_event(&mut self, event: Event, ctx: &mut AppContext) -> Transition;

    /// Draw the app's UI into the strip buffer.
    /// Called once per strip during refresh — widgets clip automatically.
    fn draw(&self, strip: &mut StripBuffer);

    /// Does this app have async work pending?
    ///
    /// Called after every event to decide whether to enqueue `AppWork`.
    /// When true, `on_work()` will be called before the next render —
    /// this preserves the render ownership invariant (no stale renders).
    fn needs_work(&self) -> bool {
        false
    }

    /// Perform async work using OS services.
    ///
    /// Called by the kernel when the `AppWork` job fires. Use `services`
    /// for I/O (directory listing, file reads) and `ctx.mark_dirty()`
    /// to request a render of changed regions.
    ///
    /// The kernel handles render scheduling — just mark what changed.
    fn on_work<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        _services: &mut Services<'_, SPI>,
        _ctx: &mut AppContext,
    ) {
        // Default: no-op
    }
}

/// App navigation stack. Fixed-size, no heap.
const MAX_STACK_DEPTH: usize = 4;

/// Describes which lifecycle methods to call after a navigation.
#[derive(Debug, Clone, Copy)]
pub struct NavEvent {
    pub from: AppId,
    pub to: AppId,
    /// If true, `from` was suspended (still on stack → call `on_suspend`).
    /// If false, `from` was removed (→ call `on_exit`).
    pub suspend: bool,
    /// If true, `to` was already on the stack (→ call `on_resume`).
    /// If false, `to` is freshly entered (→ call `on_enter`).
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
            depth: 1, // Start with Home
            ctx: AppContext::new(),
        }
    }

    /// Currently active app.
    pub fn active(&self) -> AppId {
        self.stack[self.depth - 1]
    }

    /// Apply a transition. Returns a `NavEvent` if an app switch
    /// occurred, so the main loop can call the correct lifecycle methods.
    pub fn apply(&mut self, transition: Transition) -> Option<NavEvent> {
        let old = self.active();

        let (suspend, resume) = match transition {
            Transition::None => return None,

            Transition::Push(id) => {
                if self.depth >= MAX_STACK_DEPTH {
                    // Stack full — can't preserve the old app, must replace.
                    self.stack[self.depth - 1] = id;
                    (false, false) // exit old, enter new
                } else {
                    self.stack[self.depth] = id;
                    self.depth += 1;
                    (true, false) // suspend old, enter new
                }
            }

            Transition::Pop => {
                if self.depth > 1 {
                    self.depth -= 1;
                    (false, true) // exit old, resume parent
                } else {
                    return None; // Can't pop below Home
                }
            }

            Transition::Replace(id) => {
                self.stack[self.depth - 1] = id;
                (false, false) // exit old, enter new
            }

            Transition::Home => {
                self.depth = 1;
                self.stack[0] = AppId::Home;
                (false, true) // exit old, resume Home
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

    /// Stack depth (for debug).
    pub fn depth(&self) -> usize {
        self.depth
    }
}
