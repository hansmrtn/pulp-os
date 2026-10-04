#!/usr/bin/env bash
# X4 input state-machine lock (T7). The debounce / long-press / repeat state
# machine of kernel/src/drivers/input.rs moved into pulp-board-logic
# (`input::InputCore`, shared with the OnePage C61). This proves the real X4
# source (kernel/src/drivers/input.rs, not a copy) still produces the same event
# stream, event timing and hardware-read pattern as before the move.
#
# How: host-compile the real input.rs + real board/button.rs + real
# kernel/timing.rs against an esp-hal time shim (microsecond clock) and a fake
# ADC / power pin, drive scripted scenarios (single press, sub-debounce bounce,
# 1 ms boundary scans, power priority, row2, reset_hold_state) plus a 300k-step
# pseudo-random walk, and hash the trace. PINNED_SHA256 was produced from the
# pre-T7 file (git HEAD 3bb911af kernel/src/drivers/input.rs). Re-derive it with
#   git show HEAD:kernel/src/drivers/input.rs > /tmp/input_head.rs
#   INPUT_RS=/tmp/input_head.rs bash scripts/check-x4-input-trace.sh
# (the old file, with the pin unchanged, must report this same sha256).
# A mismatch against the real file means X4 input behavior changed.
#
# Needs a host rustc and the cargo registry cache (nb, log), offline.
set -euo pipefail

PINNED_SHA256=2c3738aa158ae1a8fb60f034672119a5a5b4dba329693a7b907b8dcf2fc042e7

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/scripts/x4-input-trace"
WORK="${X4_TRACE_WORKDIR:-$(mktemp -d)}"
INPUT_RS="${INPUT_RS:-$ROOT/kernel/src/drivers/input.rs}"
mkdir -p "$WORK/esp-hal-shim/src" "$WORK/harness/src"

cp "$SRC/esp-hal-shim/Cargo.toml" "$WORK/esp-hal-shim/Cargo.toml"
cp "$SRC/esp-hal-shim/lib.rs" "$WORK/esp-hal-shim/src/lib.rs"
cp "$SRC/stim.rs" "$SRC/main.rs" "$WORK/harness/src/"
sed "s#@ROOT@#$ROOT#g" "$SRC/board_shim.rs" > "$WORK/harness/src/board_shim.rs"
cat > "$WORK/harness/Cargo.toml" <<TOML
[package]
name = "harness"
version = "0.0.0"
edition = "2024"
[dependencies]
esp-hal = { path = "../esp-hal-shim" }
pulp-board-logic = { path = "$ROOT/board-logic" }
nb = "1.1.0"
log = "0.4"
TOML
cat > "$WORK/harness/src/lib.rs" <<RS
pub mod stim;
#[path = "board_shim.rs"]
pub mod board;
pub mod kernel {
    #[path = "$ROOT/kernel/src/kernel/timing.rs"]
    pub mod timing;
}
pub mod drivers {
    #[path = "$INPUT_RS"]
    pub mod input;
}
RS
cd "$WORK/harness"
export CARGO_TARGET_DIR="$WORK/target"
OUT="$WORK/trace.txt"
cargo run -q --offline > "$OUT" 2> "$WORK/build.log" || { cat "$WORK/build.log"; echo "FAIL: harness build/run"; exit 1; }

GOT="$(sha256sum "$OUT" | cut -d' ' -f1)"
LINES="$(wc -l < "$OUT")"
if [ "$GOT" = "$PINNED_SHA256" ]; then
    echo "ok   X4 input trace identical to pre-T7 ($LINES lines, sha256 $GOT)"
else
    echo "FAIL X4 input trace changed: got $GOT, expected $PINNED_SHA256 (trace: $OUT)"
    exit 1
fi
