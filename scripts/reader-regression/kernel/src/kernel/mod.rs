// host stand-in for kernel/src/kernel/mod.rs. The real files are included as is;
// `Kernel` keeps the fields handle.rs uses, backed by the in-memory SD shim.
#[path = "../../../pulp/kernel/src/kernel/app.rs"]
pub mod app;
#[cfg(feature = "tree")]
#[path = "../../../pulp/kernel/src/kernel/bigbuf.rs"]
pub mod bigbuf;
#[path = "../../../pulp/kernel/src/kernel/bookmarks.rs"]
pub mod bookmarks;
#[path = "../../../pulp/kernel/src/kernel/config.rs"]
pub mod config;
#[path = "../../../pulp/kernel/src/kernel/handle.rs"]
pub mod handle;
#[path = "../../../pulp/kernel/src/kernel/timing.rs"]
pub mod timing;
#[path = "../../../pulp/kernel/src/kernel/work_queue.rs"]
pub mod work_queue;

pub mod dir_cache;
pub mod rtc_session {
    // placeholder for the RTC FAST session record named in AppLayer signatures
    #[derive(Clone, Copy)]
    pub struct RtcSession;
}
pub mod wake {
    pub fn uptime_secs() -> u32 {
        0
    }
}

pub use crate::drivers::storage::StorageError;
pub use crate::error::{Error, ErrorKind, Result, ResultExt};

pub use app::{
    App, AppContext, AppIdType, AppLayer, Launcher, NavEvent, PendingSetting, QuickAction,
    QuickActionKind, RECENT_FILE, Redraw, Transition,
};
#[cfg(feature = "tree")]
pub use app::SessionData;
#[cfg(feature = "tree")]
pub use bigbuf::{BigBuf, BufClass, BufError};
pub use bookmarks::BookmarkCache;
pub use handle::KernelHandle;

use crate::drivers::sdcard::SdStorage;
use dir_cache::DirCache;

pub struct Kernel {
    pub(crate) sd: SdStorage,
    pub(crate) dir_cache: &'static mut DirCache,
    pub(crate) bm_cache: &'static mut BookmarkCache,
    pub(crate) sd_ok: bool,
    pub(crate) cached_battery_mv: u16,
}

impl Kernel {
    pub fn new(sd: SdStorage) -> Self {
        let sd_ok = sd.is_mounted();
        Self {
            sd,
            dir_cache: alloc::boxed::Box::leak(alloc::boxed::Box::new(DirCache::new())),
            bm_cache: alloc::boxed::Box::leak(alloc::boxed::Box::new(BookmarkCache::new())),
            sd_ok,
            cached_battery_mv: 4000,
        }
    }

    #[inline]
    pub fn handle(&mut self) -> KernelHandle<'_> {
        KernelHandle::new(self)
    }

    pub fn sd(&self) -> &SdStorage {
        &self.sd
    }

    // what the scheduler does at boot / on housekeeping (bm_cache.ensure_loaded / flush)
    pub fn bookmarks_load(&mut self) {
        self.bm_cache.ensure_loaded(&self.sd);
    }

    pub fn bookmarks_reload(&mut self) {
        self.bm_cache.force_load(&self.sd);
    }

    pub fn bookmarks_flush(&mut self) {
        self.bm_cache.flush(&self.sd);
    }

    pub fn bookmarks(&mut self) -> &mut BookmarkCache {
        self.bm_cache
    }
}
