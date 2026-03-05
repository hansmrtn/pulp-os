// pulp-os - e-reader firmware for the XTEink X4

#![no_std]

extern crate alloc;

pub use pulp_kernel::board;
pub use pulp_kernel::drivers;
pub use pulp_kernel::error;
pub use pulp_kernel::kernel;

pub mod apps;
pub mod fonts;
pub mod ui;
