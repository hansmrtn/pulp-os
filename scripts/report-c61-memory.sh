#!/usr/bin/env bash
# R22 / R14 / R15 (T8): ELF memory report and budget check for the C61 image.
#
#   scripts/report-c61-memory.sh                report + check the C61 full firmware ELF (C61_ELF=... for the boot image)
#   scripts/report-c61-memory.sh --x4-diff A B  compare the RAM sections of two
#                                               X4 ELFs (what `size` "bss" hides)
#
# What it does (C61 mode):
#   1. prints `size -A`, allocated sections (`readelf -S`), LOAD segments
#      (`readelf -l`) and the largest static objects (`nm --size-sort`);
#   2. derives the main-RAM layout: statics (.trap .rwtext .data .bss .noinit,
#      the main heap is inside .bss) + `.stack`. `.stack` is not a fixed size:
#      the linker gives it everything between the end of the statics and the end
#      of `RAM` (`_stack_start` = 0x4083EA70 = ORIGIN(RAM)+LENGTH(RAM)), so
#      stack headroom = (RAM_LEN - statics) - STACK_MIN_BYTES;
#   3. applies the budget rules of pulp_board_logic::memory (the same code the
#      firmware links; run through the `memreport` host example):
#        - statics <= STATIC_RAM_MAX_BYTES, `.stack` last in RAM (check_image)
#        - every writable section is entirely in internal RAM (R15)
#        - every SPI DMA `BUFFER`/`DESCRIPTORS` symbol is inside internal RAM (R15)
#   4. diffs the address-map literals in board-logic against esp-hal's
#      ld/esp32c61/memory.x (skipped with a warning if the registry copy is
#      missing) and the planned heap sizes against the ELF.
# Exit status is non-zero if any check fails. Needs GNU awk (strtonum).
#
# Env: C61_TARGET_DIR (default target/t8-c61, built with `cargo build-c61
# --locked` if the ELF is missing), C61_ELF (explicit ELF path), TOP_N (default 12).
set -u
cd "$(dirname "$0")/.."

fail=0
ok()   { echo "ok    $*"; }
bad()  { echo "FAIL  $*"; fail=1; }
warn() { echo "warn  $*"; }
hr()   { echo; echo "== $* =="; }
need() { command -v "$1" >/dev/null 2>&1 || { echo "FAIL  missing tool: $1"; exit 2; }; }
need size; need readelf; need nm; need awk; need cargo
awk 'BEGIN{ if (strtonum("0x10") != 16) exit 1 }' 2>/dev/null || { echo "FAIL  awk lacks strtonum (need GNU awk)"; exit 2; }

# --- mode: compare the RAM sections of two X4 ELFs -----------------------------
if [ "${1:-}" = "--x4-diff" ]; then
  a=${2:?usage: --x4-diff A.elf B.elf}; b=${3:?usage: --x4-diff A.elf B.elf}
  hr "RAM sections: A=$a  B=$b"
  printf '%-22s %10s %10s %10s\n' section A B "B-A"
  for s in .trap .rwtext .rwdata_dummy .data .bss .noinit .rtc_fast.persistent .stack .dram2_uninit; do
    sa=$(size -A "$a" | awk -v s="$s" '$1==s{print $2}'); sb=$(size -A "$b" | awk -v s="$s" '$1==s{print $2}')
    sa=${sa:-0}; sb=${sb:-0}
    printf '%-22s %10d %10d %+10d\n' "$s" "$sa" "$sb" $((sb - sa))
  done
  echo
  echo "Berkeley 'size' reports bss = .bss + .noinit + .stack + .dram2_uninit + .rtc_fast.*"
  echo "(NOBITS sections), so a bigger .stack shows up as bigger 'bss'."
  size "$a" "$b"
  hr "largest static objects (nm --size-sort, >= 2 KiB, b/B/d/D, top 12 each)"
  for f in "$a" "$b"; do
    echo "-- $f"
    nm -C -S --size-sort "$f" | awk '$3 ~ /^[bBdD]$/ && strtonum("0x"$2) >= 2048 {printf "%8d %s %s\n", strtonum("0x"$2), $3, substr($0, index($0,$4))}' | tac | head -n 12
  done
  exit 0
fi

tdir="${C61_TARGET_DIR:-target/t8-c61}"
elf="${C61_ELF:-$tdir/riscv32imac-unknown-none-elf/release/pulp-os-c61}"
top="${TOP_N:-12}"
if [ ! -f "$elf" ]; then
  echo "building $elf ..."
  CARGO_TARGET_DIR="$tdir" cargo build-c61 --locked >"$tdir.log" 2>&1 \
    || { bad "C61 build failed (log: $tdir.log)"; exit 1; }
fi
echo "ELF: $elf ($(stat -c %s "$elf") bytes)"

# --- 1. raw tool output ---------------------------------------------------------
hr "size -A (sections with non-zero size, no debug info)"
size -A "$elf" | awk 'NR<=2 || ($2 != 0 && $1 !~ /^\.(debug|comment|riscv|symtab|strtab|shstrtab|espressif)/)'

hr "readelf -S -W (allocated sections)"
readelf -S -W "$elf" | sed 's/\[ *\([0-9]*\)\]/\1/' | awk '
  NR<=3 {print; next}
  $1 ~ /^[0-9]+$/ && $2 ~ /^\./ { fl=""; if ($8 ~ /^[WAXMSILGTCxoERDp]+$/) fl=$8; if (fl ~ /A/) print }'

hr "readelf -l -W (segments)"
readelf -l -W "$elf" | sed -n '/Program Headers/,/GNU_STACK/p;/Section to Segment/,$p' | grep -v RISCV_ATTR

# --- 2. layout facts -------------------------------------------------------------
# "name addr size flags" for allocated sections
secs=$(readelf -S -W "$elf" | sed 's/\[ *\([0-9]*\)\]/\1/' | awk '
  $1 ~ /^[0-9]+$/ && $2 ~ /^\./ {
    fl = ""; if ($8 ~ /^[WAXMSILGTCxoERDp]+$/) fl = $8
    if (fl ~ /A/) printf "%s %s %s %s\n", $2, $4, $6, fl
  }')
get() { echo "$secs" | awk -v n="$1" -v c="$2" '$1==n{print $c}'; }

ram_lo=$((0x40800000)); ram_hi=$((0x4083EA70))
static_end=0; statics=0
while read -r name addr size fl; do
  a=$((16#$addr)); s=$((16#$size))
  [ "$s" -eq 0 ] && continue
  e=$((a + s))
  if [ "$a" -ge "$ram_lo" ] && [ "$a" -lt "$ram_hi" ] && [ "$name" != ".stack" ]; then
    statics=$((statics + s)); [ "$e" -gt "$static_end" ] && static_end=$e
  fi
done <<<"$secs"
stack_addr=$((16#$(get .stack 2))); stack_size=$((16#$(get .stack 3)))

mdir=$(mktemp -d); trap 'rm -rf "$mdir"' EXIT
{
  printf 'image %x %x %x\n' "$static_end" "$stack_addr" "$stack_size"
  while read -r name addr size fl; do
    w=R; case "$fl" in *W*) w=W;; esac
    printf 'section %s %s %s %s\n' "$name" "$addr" "$size" "$w"
  done <<<"$secs"
  nm -C -S "$elf" | awk '$3 ~ /^[bBdD]$/ && $4 ~ /board_c61::spi::init::(BUFFER|DESCRIPTORS)$/ {printf "dma %s %s %s\n", $4, $1, $2}'
} >"$mdir/facts"

memreport() {
  cargo run -q -p pulp-board-logic --example memreport --target x86_64-unknown-linux-gnu \
    --config 'unstable.build-std=["std","test"]' -- "$@"
}

# --- 3. budget constants (single source: board-logic/src/memory.rs) -----------------
hr "budget constants (pulp_board_logic::memory)"
if ! memreport constants >"$mdir/consts" 2>"$mdir/consts.err"; then
  bad "memreport failed to build/run: $(tail -3 "$mdir/consts.err")"; exit 1
fi
cat "$mdir/consts"
c() { awk -F= -v k="$1" '$1==k{print $2}' "$mdir/consts"; }

# --- 4. checks by the shared rules ---------------------------------------------------
hr "checks (pulp_board_logic::memory::{check_image, section_placement_ok, range_is_internal})"
if memreport check <"$mdir/facts" >"$mdir/check.out" 2>&1; then rc=0; else rc=$?; fi
cat "$mdir/check.out"
[ "$rc" -eq 0 ] && ok "image / section / DMA rules" || bad "image / section / DMA rules (memreport exit $rc)"
ndma=$(grep -c '^dma ' "$mdir/facts")
[ "$ndma" -ge 4 ] && ok "found $ndma SPI DMA symbols (2 BUFFER + 2 DESCRIPTORS expected)" \
  || bad "expected >= 4 SPI DMA symbols, found $ndma"

# R15: PSRAM must stay off the global heap (`psram_allocator!` would add an
# External region to esp_alloc::HEAP, where plain Box/Vec could land in it)
hits=$(grep -rnE 'psram_allocator!|MemoryCapability::External' src kernel/src 2>/dev/null |
  grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' | grep -v 'kernel/src/board_c61/memory.rs' || true)
[ -z "$hits" ] && ok "source: no psram_allocator!/External region outside board_c61::memory" \
  || { bad "source: PSRAM registered outside board_c61::memory"; echo "$hits"; }
grep -cE 'External.into\(\)' kernel/src/board_c61/memory.rs | grep -qx 1 \
  && ok "source: exactly one External region registration (private PSRAM_HEAP)" \
  || bad "source: expected exactly one External region registration in board_c61::memory"
grep -qE 'PSRAM_HEAP\.add_region' kernel/src/board_c61/memory.rs \
  && ! grep -qE 'HEAP\.add_region' <(grep -v 'PSRAM_HEAP\.add_region' kernel/src/board_c61/memory.rs) \
  && ok "source: the only add_region call is PSRAM_HEAP.add_region (never esp_alloc::HEAP)" \
  || bad "source: External region registration is not limited to PSRAM_HEAP"

# stack symbols agree with the section table
ss=$(nm "$elf" | awk '$3=="_stack_start"{print $1}'); se=$(nm "$elf" | awk '$3=="_stack_end"{print $1}')
if [ $((16#${ss:-0})) -eq "$((stack_addr + stack_size))" ] && [ $((16#${se:-0})) -eq "$stack_addr" ]; then
  ok "_stack_end=0x$se _stack_start=0x$ss match the .stack section"
else
  bad "_stack_start/_stack_end ($ss/$se) disagree with the .stack section"
fi
[ $((16#${ss:-0})) -eq "$(c C61_RECLAIMED_START)" ] \
  && ok "_stack_start == C61_RECLAIMED_START (end of memory.x RAM)" \
  || bad "_stack_start != C61_RECLAIMED_START"

# planned internal heaps are really in the ELF
mainheap=$(nm -S "$elf" | awk -v n="$(c INTERNAL_HEAP_MAIN_BYTES)" '$3 ~ /^[bB]$/ && $4 ~ /HEAP$/ && strtonum("0x"$2)==n {c++} END{print c+0}')
rech=$(get .dram2_uninit 3)
[ "$mainheap" -ge 1 ] && ok "main heap static of $(c INTERNAL_HEAP_MAIN_BYTES) B present (inside .bss)" \
  || bad "no static HEAP of INTERNAL_HEAP_MAIN_BYTES in the ELF"
[ "$((16#${rech:-0}))" -eq "$(c INTERNAL_HEAP_RECLAIMED_BYTES)" ] \
  && ok ".dram2_uninit = $((16#${rech:-0})) B = INTERNAL_HEAP_RECLAIMED_BYTES (bootloader-reclaimed region)" \
  || bad ".dram2_uninit size ($((16#${rech:-0}))) != INTERNAL_HEAP_RECLAIMED_BYTES"

# no PSRAM-window addresses among writable/RAM symbols
nbad=$(nm "$elf" | awk -v lo="$(c C61_EXTMEM_START)" -v hi="$(c C61_EXTMEM_END)" '$2 ~ /^[bBdD]$/ {a=strtonum("0x"$1); if (a>=lo && a<hi) n++} END{print n+0}')
[ "$nbad" -eq 0 ] && ok "no data/bss symbol inside the flash/PSRAM window" \
  || bad "$nbad data/bss symbols inside the flash/PSRAM window"

# --- 5. memory.x literals ------------------------------------------------------------
hr "memory map literals vs esp-hal ld/esp32c61/memory.x"
mx=$(ls ~/.cargo/registry/src/*/esp-hal-1.2.0/ld/esp32c61/memory.x 2>/dev/null | head -1)
if [ -n "$mx" ]; then
  echo "memory.x: $mx"
  ram_o=$(awk '/RAM : ORIGIN/ {for(i=1;i<=NF;i++){if($i=="ORIGIN"){print $(i+2)}}}' "$mx" | tr -d ',')
  ram_l=$(awk '/RAM : ORIGIN/ {for(i=1;i<=NF;i++){if($i=="LENGTH"){print $(i+2)}}}' "$mx")
  dram_end=$(awk '/dram2_seg/ {for(i=1;i<=NF;i++){if($i ~ /^0x4084/){print $i; exit}}}' "$mx")
  drom=$(awk '/"DROM"/ {gsub(/[\[\],"]/,""); print $1, $2}' "$mx")
  [ $((ram_o)) -eq "$(c C61_RAM_START)" ] && ok "RAM ORIGIN $ram_o" || bad "RAM ORIGIN $ram_o != C61_RAM_START"
  [ $((ram_l)) -eq "$(c C61_RAM_LEN)" ] && ok "RAM LENGTH $ram_l" || bad "RAM LENGTH $ram_l != C61_RAM_LEN"
  [ $((dram_end)) -eq "$(c C61_RAM_END)" ] && ok "dram2 end $dram_end" || bad "dram2 end $dram_end != C61_RAM_END"
  set -- $drom
  if [ $(($1)) -eq "$(c C61_EXTMEM_START)" ] && [ $(($2)) -eq "$(c C61_EXTMEM_END)" ]; then
    ok "DROM window $1..$2"
  else
    bad "DROM window $drom != C61_EXTMEM_START..END"
  fi
else
  warn "esp-hal-1.2.0 memory.x not in the cargo registry; literal check skipped"
fi

# --- 6. totals -------------------------------------------------------------------------
hr "internal RAM summary"
ram_len=$(c C61_RAM_LEN); rec=$(c C61_RECLAIMED_LEN)
dram2=$((16#${rech:-0}))
img=0
while read -r name addr size fl; do
  s=$((16#$size))
  case "$name" in .trap|.rwtext|.data|.rodata|.text|.flash.appdesc) img=$((img + s));; esac
done <<<"$secs"
printf '%-52s %9d B\n' "HP SRAM usable (memory.x RAM + dram2_seg)" $((ram_len + rec))
printf '%-52s %9d B\n' "  RAM (linker main region)" "$ram_len"
printf '%-52s %9d B  (%d.%d%% of RAM)\n' "    statics (.trap .rwtext .data .bss .noinit)" "$statics" $((statics * 100 / ram_len)) $((statics * 1000 / ram_len % 10))
printf '%-52s %9d B\n' "    .stack (= RAM - statics)" "$stack_size"
printf '%-52s %9d B\n' "    STACK_MIN_BYTES" "$(c STACK_MIN_BYTES)"
printf '%-52s %9d B\n' "    stack headroom over STACK_MIN_BYTES" $((stack_size - $(c STACK_MIN_BYTES)))
printf '%-52s %9d B\n' "    statics budget (STATIC_RAM_MAX_BYTES)" "$(c STATIC_RAM_MAX_BYTES)"
printf '%-52s %9d B\n' "    statics budget left" $(($(c STATIC_RAM_MAX_BYTES) - statics))
printf '%-52s %9d B  (%d%% of dram2_seg)\n' "  dram2_seg used by .dram2_uninit (heap)" "$dram2" $((dram2 * 100 / rec))
printf '%-52s %9d B\n' "image bytes (.text .rodata .rwtext .trap .data .appdesc)" "$img"
echo "note: Berkeley 'size' counts .stack in 'text' on this target (section flags A,"
echo "      not W), so its text column is not a code-size figure; use the line above."
echo "      The actual stack use (high water mark) is NOT measured: hardware only."

hr "PSRAM plan (2 MiB part; not verified on hardware)"
printf '%-34s %9d B\n' "PSRAM_HW_BYTES" "$(c PSRAM_HW_BYTES)"
printf '%-34s %9d B\n' "  chapter text limit" "$(c PSRAM_CHAPTER_TEXT_BYTES)"
printf '%-34s %9d B\n' "  image data limit" "$(c PSRAM_IMAGE_DATA_BYTES)"
printf '%-34s %9d B\n' "  page table limit" "$(c PSRAM_PAGE_TABLE_BYTES)"
printf '%-34s %9d B\n' "  zip/toc limit" "$(c PSRAM_ZIP_TOC_BYTES)"
printf '%-34s %9d B\n' "  reserve (allocator/headroom)" "$(c PSRAM_RESERVE_BYTES)"
printf '%-34s %9d B\n' "internal heap (main + reclaimed)" "$(c INTERNAL_HEAP_BYTES)"

hr "largest static objects (nm --size-sort, b/B/d/D, top $top)"
nm -C -S --size-sort "$elf" | awk '$3 ~ /^[bBdD]$/ {printf "%8d  %s  %s  %s\n", strtonum("0x"$2), $1, $3, substr($0, index($0,$4))}' | tac | head -n "$top"

echo
if [ "$fail" -eq 0 ]; then echo "RESULT: all memory budget checks ok"; else echo "RESULT: memory budget check FAILED"; fi
exit "$fail"
