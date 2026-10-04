// Host build of the pulp-kernel sources that the English TXT/EPUB reader path
// uses. Everything under `pulp/` is the real, unmodified source file of the tree
// under test (`#[path]`); the modules written here are the hardware edge only
// (see Cargo.toml.in). Module names and paths match the firmware crate so the
// real files' `crate::...` paths resolve unchanged.
#![allow(dead_code, unused_imports, unused_variables)]

extern crate alloc;

#[path = "../../pulp/kernel/src/error.rs"]
pub mod error;
#[path = "../../pulp/kernel/src/util/mod.rs"]
pub mod util;
#[path = "../../pulp/kernel/src/ui/mod.rs"]
pub mod ui;

pub mod board;
pub mod drivers;
pub mod kernel;

pub use error::{Error, ErrorKind, Result, ResultExt};
