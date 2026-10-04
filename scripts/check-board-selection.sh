#!/usr/bin/env bash
# R2 check: invalid board selections must fail at build time with a readable
# message (compile_error! in kernel/src/lib.rs). Positive builds are covered
# by `cargo build-x4` / `cargo build-c61`.
set -u
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/board-selection-check}"

fail=0
expect_error() { # <description> <expected substring> <cargo args...>
  local desc=$1 want=$2
  shift 2
  local out
  if out=$(cargo build --release --locked "$@" 2>&1); then
    echo "FAIL  $desc: build unexpectedly succeeded"; fail=1; return
  fi
  if grep -qF -- "$want" <<<"$out"; then
    echo "ok    $desc"
  else
    echo "FAIL  $desc: expected message not found: $want"; fail=1
  fi
}

expect_error "no board"            "no board selected"
expect_error "both boards"         "multiple boards selected" \
  --features board-x4,board-onepage-c61
expect_error "x4 on imac target"   "feature \`board-x4\` requires --target riscv32imc-unknown-none-elf" \
  --target riscv32imac-unknown-none-elf --features board-x4
expect_error "c61 on imc target"   "feature \`board-onepage-c61\` requires --target riscv32imac-unknown-none-elf" \
  --target riscv32imc-unknown-none-elf --features board-onepage-c61

exit $fail
