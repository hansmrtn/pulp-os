#!/usr/bin/env bash
# R8 check (T5): the C61 build must not depend on the C3-only raw GPIO register
# path (kernel/src/board/raw_gpio.rs: GPIO_OUT_W1TS 0x6000_4008, GPIO_ENABLE
# 0x6000_4024, IO_MUX 0x6000_9000, out-sel 0x6000_4554, used for X4's GPIO12
# SD CS). Four independent probes:
#   1. source: no C3 raw register constants / raw_gpio users in any file the
#      C61 build compiles;
#   2. structure: `mod board;` (which owns raw_gpio) is gated to board-x4;
#   3. ELF symbols: no raw_gpio / RawOutputPin symbols in the C61 ELF;
#   4. ELF code: no `lui rd,0x60004` (C3 GPIO block) in the C61 ELF. (A
#      `lui rd,0x60009` is allowed only if it is a C61 PAC peripheral: on the
#      esp32c61 PAC 0x6000_9000 is TIMG1, not the C3 IO_MUX.)
# Positive control: the X4 ELF (if present) must trip probes 3 and 4.
#
# Env: C61_TARGET_DIR (default target/t5-c61), X4_ELF (optional control).
set -u
cd "$(dirname "$0")/.."
tdir="${C61_TARGET_DIR:-target/t5-c61}"
rel="$tdir/riscv32imac-unknown-none-elf/release"
elf="$rel/pulp-os-c61"       # full firmware (T12)
elf_boot="$rel/pulp-os-c61-boot"
fail=0
ok()  { echo "ok    $*"; }
bad() { echo "FAIL  $*"; fail=1; }

if [ ! -f "$elf" ] || [ ! -f "$elf_boot" ]; then
  CARGO_TARGET_DIR="$tdir" cargo build-c61 --locked >"$tdir.log" 2>&1 \
    || { bad "C61 build failed (log: $tdir.log)"; exit 1; }
fi

# 1. source probe over the C61 compile set
SRC=(kernel/src/board_c61 board-logic/src kernel/src/drivers/sdcard.rs
     kernel/src/drivers/storage.rs kernel/src/drivers/mod.rs kernel/src/error.rs
     src/bin/c61_boot.rs src/bin/main_c61.rs
     # T12: the full firmware also compiles the scheduler, the apps and the UI
     kernel/src/kernel kernel/src/ui kernel/src/util kernel/src/drivers/strip.rs
     src/apps src/ui)
RE='0x6000_?[0-9a-fA-F]{4}|raw_gpio|RawOutputPin|GPIO_OUT_W1T[SC]|GPIO_ENABLE_W1TS|IO_MUX_BASE'
hits=$(grep -rnE "$RE" "${SRC[@]}" | grep -vE '^[^:]+:[0-9]+:\s*(//|///)' || true)
[ -z "$hits" ] && ok "source: no C3 raw register constants / raw_gpio users in the C61 compile set" \
  || { bad "source: C3 raw GPIO references in the C61 compile set"; echo "$hits"; }

# 2. raw_gpio is reachable only through the X4-gated board module
if grep -B2 '^pub mod board;' kernel/src/lib.rs | grep -q 'feature = "board-x4"'; then
  ok "structure: kernel mod board (owner of raw_gpio) is cfg(board-x4)"
else
  bad "structure: kernel mod board is not gated to board-x4"
fi
grep -q '^pub mod raw_gpio;' kernel/src/board/mod.rs \
  && ok "structure: raw_gpio only declared inside board/mod.rs" \
  || bad "structure: raw_gpio declaration moved"

probe_elf() { # <label> <elf> <expect: absent|present>
  local label=$1 f=$2 want=$3 nsym nlui
  nsym=$(nm -C "$f" 2>/dev/null | grep -ciE 'raw_gpio|RawOutputPin')
  nlui=$(objdump -d --no-show-raw-insn "$f" 2>/dev/null | grep -cE 'lui[[:space:]]+[a-z0-9]+,0x60004\b')
  if [ "$want" = absent ]; then
    [ "$nsym" -eq 0 ] && ok "$label: no raw_gpio/RawOutputPin symbols" || bad "$label: $nsym raw_gpio symbols"
    [ "$nlui" -eq 0 ] && ok "$label: no lui 0x60004 (C3 GPIO block) in code" || bad "$label: $nlui lui 0x60004 in code"
  else
    [ "$nsym" -gt 0 ] && ok "$label: control finds raw_gpio symbols ($nsym)" || bad "$label: control probe finds nothing (probe broken)"
    [ "$nlui" -gt 0 ] && ok "$label: control finds lui 0x60004 ($nlui)" || bad "$label: control lui probe finds nothing (probe broken)"
  fi
}

# 3 + 4. ELF probes
for e in "$elf" "$elf_boot"; do
  label="c61 $(basename "$e")"
  probe_elf "$label" "$e" absent
  nlui9=$(objdump -d --no-show-raw-insn "$e" 2>/dev/null | grep -cE 'lui[[:space:]]+[a-z0-9]+,0x60009\b')
  echo "info  $label: $nlui9 x lui 0x60009 (expected: esp32c61 PAC TIMG1 @0x6000_9000, not C3 IO_MUX)"
done

if [ -n "${X4_ELF:-}" ] && [ -f "$X4_ELF" ]; then
  probe_elf "x4 elf (control)" "$X4_ELF" present
fi

exit $fail
