#!/usr/bin/env bash
# X4 battery equivalence lock (T9). The voltage -> percent algorithm and the
# discharge table moved from kernel/src/{drivers,board}/battery.rs into
# pulp-board-logic (`battery`, shared with the OnePage C61). This proves the
# REAL X4 files (not copies) give the same adc_to_battery_mv() and
# battery_percentage() for every u16 input as the files at git HEAD (pre-T9).
#
# How: host-compile the pre-T9 files (git show HEAD:...) and the working-tree
# files into two harness builds, print "mv adc_to_battery_mv percentage" for
# 0..=65535, and compare the two outputs (sha256 + diff). PINNED_SHA256 is the
# hash of the pre-T9 output; the new files must reproduce it.
#
# Needs a host rustc and git. No registry access: the only dependency is the
# zero-dependency board-logic path crate.
set -euo pipefail

PINNED_SHA256=${PINNED_SHA256:-272e9905ce250cf669baece86834b14f32607d08a7bd81a0416d1aad70925184}

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/scripts/x4-battery-equiv"
WORK="${X4_BATTERY_WORKDIR:-$(mktemp -d)}"
mkdir -p "$WORK"

git -C "$ROOT" show HEAD:kernel/src/drivers/battery.rs > "$WORK/old_driver.rs"
git -C "$ROOT" show HEAD:kernel/src/board/battery.rs > "$WORK/old_board.rs"

run() { # name driver.rs board.rs -> prints sha256 of the output
    local name="$1" drv="$2" brd="$3" dir="$WORK/$1"
    mkdir -p "$dir/src"
    cp "$SRC/main.rs" "$dir/src/main.rs"
    cat > "$dir/Cargo.toml" <<TOML
[package]
name = "harness"
version = "0.0.0"
edition = "2024"
[lib]
path = "src/lib.rs"
[dependencies]
pulp-board-logic = { path = "$ROOT/board-logic" }
TOML
    cat > "$dir/src/lib.rs" <<RS
pub mod board {
    #[path = "$brd"]
    pub mod battery;
}
pub mod drivers {
    #[path = "$drv"]
    pub mod battery;
}
RS
    (cd "$dir" && CARGO_TARGET_DIR="$WORK/target-$name" cargo run -q --offline \
        --target x86_64-unknown-linux-gnu > "$WORK/$name.txt" 2> "$WORK/$name.log") \
        || { cat "$WORK/$name.log"; echo "FAIL: harness $name build/run"; exit 1; }
    sha256sum "$WORK/$name.txt" | cut -d' ' -f1
}

OLD="$(run old "$WORK/old_driver.rs" "$WORK/old_board.rs")"
NEW="$(run new "$ROOT/kernel/src/drivers/battery.rs" "$ROOT/kernel/src/board/battery.rs")"
LINES="$(wc -l < "$WORK/new.txt")"

if [ -n "$PINNED_SHA256" ] && [ "$OLD" != "$PINNED_SHA256" ]; then
    echo "FAIL pre-T9 reference output changed: $OLD vs pin $PINNED_SHA256"
    exit 1
fi
if [ "$OLD" = "$NEW" ]; then
    echo "ok   X4 battery conversions identical to pre-T9 ($LINES inputs, sha256 $NEW)"
else
    echo "FAIL X4 battery conversions changed (old $OLD, new $NEW)"
    diff "$WORK/old.txt" "$WORK/new.txt" | head -20 || true
    exit 1
fi
