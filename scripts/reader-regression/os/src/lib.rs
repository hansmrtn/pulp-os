// Host build of the pulp-os app layer pieces that handle English TXT/EPUB:
// the real reader (src/apps/reader/*), settings app, widgets, fonts and ui,
// compiled against the real kernel sources (crate pulp-kernel, hardware edge
// shimmed). Same crate-root names as src/lib.rs so `crate::...` paths in the
// real files resolve unchanged.
#![allow(dead_code, unused_imports, unused_variables, unused_mut)]

extern crate alloc;

pub use pulp_kernel::error;
pub use pulp_kernel::{board, drivers, kernel};

pub mod apps;
#[path = "../../pulp/src/fonts/mod.rs"]
pub mod fonts;
#[path = "../../pulp/src/ui/mod.rs"]
pub mod ui;

pub mod fixtures;
pub mod golden;
pub mod rig;
