// pulp-os - e-reader firmware for the XTEink X4

#![no_std]

extern crate alloc;

// Both boards build the full firmware library (T12). The board-specific parts
// are behind `pulp_kernel::board` (X4: kernel/src/board/, C61:
// kernel/src/board_c61/api.rs); the apps are written against that surface.
pub use pulp_kernel::error;
pub use pulp_kernel::{board, drivers, kernel};

pub mod apps;
pub mod fonts;
pub mod ui;

#[cfg(all(feature = "wifi", feature = "board-onepage-c61"))]
compile_error!(
    "feature `wifi` is not ported to board-onepage-c61 yet; the OnePage C61 firmware is offline-only (use `cargo build-c61`)."
);
