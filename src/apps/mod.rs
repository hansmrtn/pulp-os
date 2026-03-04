// app modules, AppId definition, and re-exports from kernel::app
//
// AppId is defined here (the distro side) -- the kernel is generic
// over AppIdType and never knows which concrete apps exist.

pub mod files;
pub mod home;
pub mod manager;
pub mod reader;
pub mod widgets;

pub mod settings;
pub mod upload;

use crate::kernel::app::AppIdType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppId {
    Home,
    Files,
    Reader,
    Settings,
    // upload bypasses the App trait; AppManager::needs_special_mode
    // returns true for this variant and run_special_mode handles it
    Upload,
}

impl AppIdType for AppId {
    const HOME: Self = Self::Home;
}

pub type Transition = crate::kernel::app::Transition<AppId>;
pub type NavEvent = crate::kernel::app::NavEvent<AppId>;
pub type Launcher = crate::kernel::app::Launcher<AppId>;

pub use crate::kernel::app::{App, AppContext, PendingSetting, RECENT_FILE, Redraw};

// Unified error types — available to all app code as `crate::apps::Error` etc.
pub use crate::kernel::{Error, ErrorKind, Result, ResultExt};

// Backward-compatible alias; old app code referencing `StorageError`
// keeps compiling — it is now `type StorageError = Error`.
pub use crate::kernel::StorageError;
