// pulp-kernel -- hardware drivers, scheduling, and system core
//
// generic over AppLayer; never imports concrete apps or fonts
// ships a built-in mono font (FONT_9X18) for boot console and
// sleep screen; distros bring their own proportional fonts

#![no_std]

extern crate alloc;

// board selection: exactly one board feature must be enabled, and it must
// match the build target (the chip crates are chosen by target, see the
// root Cargo.toml). Use `cargo build-x4` / `cargo build-c61`.
#[cfg(not(any(feature = "board-x4", feature = "board-onepage-c61")))]
compile_error!(
    "no board selected. Enable exactly one board feature: `board-x4` (Xteink X4, esp32c3, target riscv32imc-unknown-none-elf) or `board-onepage-c61` (OnePage C61, esp32c61, target riscv32imac-unknown-none-elf). Use the cargo aliases `cargo build-x4` / `cargo build-c61` (see README.txt)."
);

#[cfg(all(feature = "board-x4", feature = "board-onepage-c61"))]
compile_error!(
    "multiple boards selected. Features `board-x4` and `board-onepage-c61` are mutually exclusive; enable exactly one of them (check `--features` and `--all-features`)."
);

// `a` (atomics) is the only difference between the two targets
#[cfg(all(
    feature = "board-x4",
    not(feature = "board-onepage-c61"),
    target_feature = "a"
))]
compile_error!(
    "feature `board-x4` requires --target riscv32imc-unknown-none-elf (esp32c3), but this target has the A extension. Use `cargo build-x4`."
);

#[cfg(all(
    feature = "board-onepage-c61",
    not(feature = "board-x4"),
    not(target_feature = "a")
))]
compile_error!(
    "feature `board-onepage-c61` requires --target riscv32imac-unknown-none-elf (esp32c61), but this target has no A extension. Use `cargo build-c61`."
);

// Hardware-facing modules. `board` is the X4 board support (written against
// the esp32c3 HAL surface); on the OnePage C61 the same path is a thin
// board-neutral surface over `board_c61` (screen size, key mapper, display
// type) so the kernel and the apps compile unchanged against `crate::board::*`.
#[cfg(feature = "board-x4")]
pub mod board;
// OnePage C61 board support, built up task by task (T4-T11) and integrated in
// T12 (`board_c61::api`, `board_c61::hw`). Pure logic lives in pulp-board-logic.
#[cfg(feature = "board-onepage-c61")]
pub mod board_c61;
#[cfg(feature = "board-onepage-c61")]
pub use board_c61::api as board;
pub mod drivers;
pub mod error;
pub mod kernel;
pub mod ui;
pub mod util;

// re-export core error types at crate root
pub use error::{Error, ErrorKind, Result, ResultExt};
