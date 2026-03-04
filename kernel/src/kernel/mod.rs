// kernel: owns hardware resources, caches, and system state
//
// constructed once during boot in main(), lives for the lifetime of
// the program. not a separate Embassy task -- a struct held by main.
//
// apps interact exclusively through KernelHandle, which borrows the
// kernel for the duration of an async lifecycle method.

pub mod app;
pub mod bookmarks;
pub mod config;
pub mod console;
pub mod dir_cache;
pub mod handle;
pub mod scheduler;
pub mod tasks;
pub mod wake;
pub mod work_queue;

// Unified error types (primary home: crate::error)
pub use crate::error::{Error, ErrorKind, Result, ResultExt};

// Backward-compatible alias so `kernel::StorageError` keeps working
// during migration.  It is now `type StorageError = Error`.
pub use crate::drivers::storage::StorageError;

pub use app::{
    App, AppContext, AppIdType, AppLayer, Launcher, NavEvent, PendingSetting, QuickAction,
    QuickActionKind, RECENT_FILE, Redraw, Transition,
};
pub use bookmarks::BookmarkCache;
pub use console::BootConsole;
pub use handle::KernelHandle;
pub use wake::uptime_secs;

use esp_hal::delay::Delay;

use crate::board::Epd;
use crate::drivers::sdcard::SdStorage;
use crate::drivers::strip::StripBuffer;
use crate::kernel::dir_cache::DirCache;

// default ghost-clear interval (overridden by settings once loaded)
pub const DEFAULT_GHOST_CLEAR_EVERY: u32 = 10;

pub struct Kernel {
    pub(crate) sd: SdStorage,
    pub(crate) dir_cache: &'static mut DirCache,
    pub(crate) bm_cache: &'static mut BookmarkCache,
    pub(crate) epd: Epd,
    pub(crate) strip: &'static mut StripBuffer,
    pub(crate) delay: Delay,
    pub(crate) sd_ok: bool,
    pub(crate) cached_battery_mv: u16,
    pub(crate) partial_refreshes: u32,

    // true when RED RAM is out of sync with BW after a skipped
    // phase3_sync (rapid navigation); next partial uses inv_red
    pub(crate) red_stale: bool,
}

impl Kernel {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sd: SdStorage,
        epd: Epd,
        strip: &'static mut StripBuffer,
        dir_cache: &'static mut DirCache,
        bm_cache: &'static mut BookmarkCache,
        delay: Delay,
        sd_ok: bool,
        battery_mv: u16,
    ) -> Self {
        Self {
            sd,
            dir_cache,
            bm_cache,
            epd,
            strip,
            delay,
            sd_ok,
            cached_battery_mv: battery_mv,
            partial_refreshes: 0,
            red_stale: false,
        }
    }

    #[inline]
    pub fn handle(&mut self) -> KernelHandle<'_> {
        KernelHandle::new(self)
    }

    #[inline]
    pub fn set_battery_mv(&mut self, mv: u16) {
        self.cached_battery_mv = mv;
    }

    #[inline]
    pub fn reset_partial_count(&mut self) {
        self.partial_refreshes = 0;
        self.red_stale = false;
    }

    #[inline]
    pub fn bump_partial_count(&mut self) {
        self.partial_refreshes += 1;
    }
}
