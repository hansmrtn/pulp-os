#!/usr/bin/env bash
# English TXT/EPUB regression (T13, R21): host tests of the REAL reader, settings,
# bookmark and kernel-handle sources.
#
# How: scripts/reader-regression/ is a host harness (two crates + an esp-hal stub)
# that `#[path]`-includes the actual source files of the tree under test
# (src/apps/reader/*, settings.rs, widgets, fonts, ui, kernel/src/{error,ui,util,
# board/{action,button,layout},drivers/strip,kernel/{app,bigbuf,bookmarks,config,
# handle,timing,work_queue}}.rs and the real build.rs font generator). Only the
# hardware edge is shimmed: the SD card (in-memory file store with the same function
# names/semantics as drivers/storage.rs), the Kernel struct and two type aliases.
# There is no test copy of pager / settings / bookmark logic.
#
# Variants:
#   tree  the work tree (post-port code)                         [default]
#   head  `git archive $BASE_REV` = the pre-port X4 code, built in the same harness
# The same tests and the same golden trace run on both; the golden sha256 below is
# produced by the head variant and must be reproduced by the tree variant.
#
# Usage: scripts/check-reader-regression.sh [tree|head|both] [--golden-only] [--filter X]
# Env:   READER_REG_WORKDIR (default /tmp/pulp-reader-regression)
#        READER_REG_SRC     override the tree source root (used by the mutant runner)
#        BASE_REV           pre-port commit (default 3bb911af)
set -euo pipefail

BASE_REV="${BASE_REV:-3bb911af}"
# sha256 of `golden` stdout, produced from BASE_REV (see baseline.md, T13)
PINNED_SHA256="${PINNED_SHA256:-8cb6e31a70a32cdc05d30fab45ee0c7983da8cde6a55ba8bb45a0540f8d3f454}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SMOL="$(cd "$ROOT/../smol-epub" && pwd)"
WORKROOT="${READER_REG_WORKDIR:-${TMPDIR:-/tmp}/pulp-reader-regression}"
TOOLCHAIN="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml")"
CARGO=(cargo "+$TOOLCHAIN")

VARIANTS=tree
GOLDEN_ONLY=0
FILTER=()
while [ $# -gt 0 ]; do
    case "$1" in
        tree|head|both) VARIANTS="$1" ;;
        --golden-only) GOLDEN_ONLY=1 ;;
        --filter) shift; FILTER=("$1") ;;
        *) echo "unknown arg $1" >&2; exit 2 ;;
    esac
    shift
done
[ "$VARIANTS" = both ] && VARIANTS="head tree"

prepare() { # <variant> -> sets WORK, SRC, FEATURES
    local v=$1
    WORK="$WORKROOT/$v"
    mkdir -p "$WORK"
    if [ "$v" = head ]; then
        SRC="$WORK/src-$BASE_REV"
        if [ ! -f "$SRC/.exported" ]; then
            rm -rf "$SRC"; mkdir -p "$SRC"
            git -C "$ROOT" archive "$BASE_REV" | tar -x -C "$SRC"
            touch "$SRC/.exported"
        fi
        FEATURES="board-x4"
    else
        SRC="${READER_REG_SRC:-$ROOT}"
        FEATURES="board-x4,tree"
    fi
    rm -rf "$WORK/esp-hal-shim" "$WORK/kernel" "$WORK/os"
    cp -r "$ROOT/scripts/reader-regression/esp-hal-shim" "$WORK/esp-hal-shim"
    cp -r "$ROOT/scripts/reader-regression/kernel" "$WORK/kernel"
    cp -r "$ROOT/scripts/reader-regression/os" "$WORK/os"
    cp "$ROOT/scripts/reader-regression/Cargo.toml.in" "$WORK/Cargo.toml"
    for f in kernel/Cargo.toml os/Cargo.toml; do
        sed -e "s#@ROOT@#$ROOT#g" -e "s#@SMOL@#$SMOL#g" "$WORK/$f.in" > "$WORK/$f"
        rm "$WORK/$f.in"
    done
    sed -e "s#@SRC@#$SRC#g" "$WORK/os/build.rs.in" > "$WORK/os/build.rs"
    rm "$WORK/os/build.rs.in"
    ln -sfn "$SRC" "$WORK/pulp"
    ln -sfn "$SRC/assets" "$WORK/os/assets"
}

run_variant() {
    local v=$1 out sha
    prepare "$v"
    echo "== reader regression [$v] src=$SRC features=$FEATURES"
    cd "$WORK"
    export CARGO_TARGET_DIR="$WORK/target"
    unset CARGO_BUILD_TARGET RUSTFLAGS
    if [ "$GOLDEN_ONLY" = 0 ]; then
        if ! "${CARGO[@]}" test --offline -p pulp-os-host --features "$FEATURES" "${FILTER[@]}" > "$WORK/test.log" 2>&1; then
            grep -E "^(error|test .*FAILED|---- |thread .*panicked)" "$WORK/test.log" | head -40 || true
            echo "FAIL reader regression tests [$v] (log: $WORK/test.log)"; exit 1
        fi
        local passed
        passed="$(grep -E "^test result: ok" "$WORK/test.log" | sed -E 's/^test result: ok\. ([0-9]+) passed.*/\1/' | awk '{n+=$1} END {print n+0}')"
        if [ "${passed:-0}" -lt 1 ]; then echo "FAIL no test ran [$v]"; exit 1; fi
        echo "tests [$v]: $passed passed ($(grep -c '^test .* ok$' "$WORK/test.log") named results)"
    fi
    out="$WORK/golden.txt"
    "${CARGO[@]}" run -q --offline -p pulp-os-host --features "$FEATURES" --bin golden > "$out" 2> "$WORK/golden.err" || { cat "$WORK/golden.err"; echo "FAIL golden run [$v]"; exit 1; }
    sha="$(sha256sum "$out" | cut -d' ' -f1)"
    echo "golden [$v]: $(wc -l < "$out") lines, sha256 $sha"
    GOLDEN_SHA="$sha"
    if [ "$sha" != "$PINNED_SHA256" ]; then
        echo "FAIL golden trace [$v] differs from pre-port pin $PINNED_SHA256 (trace: $out)"; exit 1
    fi
    echo "ok   reader regression [$v]"
}

for v in $VARIANTS; do run_variant "$v"; done
