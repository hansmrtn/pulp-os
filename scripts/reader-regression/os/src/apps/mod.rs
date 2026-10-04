// app registry for the host build. Mirrors src/apps/mod.rs minus the apps that
// are not part of this regression (files, home, manager, upload); the AppId enum
// and the aliases below are the only lines duplicated from it.
#[path = "../../../pulp/src/apps/reader/mod.rs"]
pub mod reader;
#[path = "../../../pulp/src/apps/settings.rs"]
pub mod settings;
#[path = "../../../pulp/src/apps/widgets/mod.rs"]
pub mod widgets;

pub mod probe;

use crate::kernel::app::AppIdType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppId {
    Home,
    Files,
    Reader,
    Settings,
}

impl AppIdType for AppId {
    const HOME: Self = Self::Home;
}

pub type Transition = crate::kernel::app::Transition<AppId>;
pub type NavEvent = crate::kernel::app::NavEvent<AppId>;
pub type Launcher = crate::kernel::app::Launcher<AppId>;

pub use crate::kernel::app::{App, AppContext, PendingSetting, RECENT_FILE, Redraw};
pub use crate::kernel::{Error, ErrorKind, Result, ResultExt};
pub use crate::kernel::StorageError;
