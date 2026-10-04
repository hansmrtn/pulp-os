#!/usr/bin/env bash
# X4 EPD driver wire-trace lock (T6). The X4 driver's pure sequences and the
# strip layout moved into pulp-board-logic; this proves the real X4 driver
# source (kernel/src/drivers/{ssd1677,strip}.rs, unmodified, not a copy) still
# emits the byte-for-byte same SPI/DC/RST/delay trace as before the move.
#
# How: build the two kernel files on the host against a tiny esp-hal shim
# (Delay + Instant only) and fakes for SPI/DC/RST/BUSY, drive init, a full
# frame, 7 partial regions (both phase-1 variants, phase 3), deep sleep, and
# hash the recorded trace. PINNED_SHA256 was produced from the pre-T6 sources
# (git HEAD 3bb911af kernel/src/drivers/{ssd1677,strip}.rs), so a mismatch means
# X4 behavior changed. Update the pin only for an intentional X4 change.
#
# Needs a host rustc (any recent stable; the repo's riscv nightly is not
# involved: the temp crate lives outside the repo) and the cargo registry
# cache (embedded-hal, embedded-hal-async, embedded-graphics-core, log).
set -euo pipefail

PINNED_SHA256=66a0447f8519e26704fda427c2fe1da4ad58bd6ae2a618214b0dfe16f6cc6ffb

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/scripts/x4-driver-trace"
WORK="${X4_TRACE_WORKDIR:-$(mktemp -d)}"
mkdir -p "$WORK/esp-hal-shim/src" "$WORK/harness/src"

cp "$SRC/esp-hal-shim/Cargo.toml" "$WORK/esp-hal-shim/Cargo.toml"
cp "$SRC/esp-hal-shim/lib.rs" "$WORK/esp-hal-shim/src/lib.rs"
cp "$SRC/stim.rs" "$SRC/main.rs" "$SRC/ui.rs" "$WORK/harness/src/"
cat > "$WORK/harness/Cargo.toml" <<TOML
[package]
name = "harness"
version = "0.0.0"
edition = "2024"
[dependencies]
esp-hal = { path = "../esp-hal-shim" }
pulp-board-logic = { path = "$ROOT/board-logic" }
embedded-hal = "1.0.0"
embedded-hal-async = "1.0.0"
embedded-graphics-core = "0.4.1"
log = "0.4"
TOML
cat > "$WORK/harness/src/lib.rs" <<RS
pub mod ui;
pub mod stim;
pub mod drivers {
    #[path = "$ROOT/kernel/src/drivers/ssd1677.rs"]
    pub mod ssd1677;
    #[path = "$ROOT/kernel/src/drivers/strip.rs"]
    pub mod strip;
}
RS
# stim.rs/main.rs refer to the crate as `harness`
cd "$WORK/harness"
export CARGO_TARGET_DIR="$WORK/target"
OUT="$WORK/trace.txt"
cargo run -q --offline > "$OUT" 2> "$WORK/build.log" || { cat "$WORK/build.log"; echo "FAIL: harness build/run"; exit 1; }

GOT="$(sha256sum "$OUT" | cut -d' ' -f1)"
LINES="$(wc -l < "$OUT")"
if [ "$GOT" = "$PINNED_SHA256" ]; then
    echo "ok   X4 driver trace identical to pre-T6 ($LINES lines, sha256 $GOT)"
else
    echo "FAIL X4 driver trace changed: got $GOT, expected $PINNED_SHA256 (trace: $OUT)"
    exit 1
fi
