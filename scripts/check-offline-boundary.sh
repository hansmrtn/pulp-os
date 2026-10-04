#!/usr/bin/env bash
# R4/R5 check: the offline firmware (feature `wifi` off, the default) must not
# link any radio / network stack and must not carry the upload menu entry.
# The X4 build with `--features wifi` is the positive control: the same probes
# must find the radio symbols and the Upload strings there, otherwise the
# probes themselves are broken.
#
# Builds X4 offline, X4 + wifi, and C61 (offline-only) into separate
# CARGO_TARGET_DIRs under target/ (override with OFFLINE_CHECK_TARGET_ROOT).
set -u
cd "$(dirname "$0")/.."
root="${OFFLINE_CHECK_TARGET_ROOT:-target/offline-boundary}"
fail=0
ok()   { echo "ok    $*"; }
bad()  { echo "FAIL  $*"; fail=1; }

X4T=riscv32imc-unknown-none-elf
C61T=riscv32imac-unknown-none-elf
RADIO_CRATES=(esp-radio esp-radio-rtos-driver embassy-net smoltcp esp-wifi-sys)
# Symbol patterns of the radio driver / network stack. Absolute symbols (nm
# type A) are ignored: esp-hal's ROM linker scripts define e.g.
# esp_wifi_cert_tx_* as fixed ROM addresses on C61, which is not linked code.
SYM_RE='esp_radio|esp_wifi|embassy_net|smoltcp'
# UI / server strings that exist only in upload mode
STR_RE='pulp\.local|WiFi config error!|No WiFi credentials!|Connection failed!'

build() { # <target dir> <cargo args...>
  local dir=$1; shift
  if ! CARGO_TARGET_DIR="$dir" cargo "$@" --locked >"$dir.log" 2>&1; then
    bad "build failed: cargo $* (log: $dir.log)"; tail -5 "$dir.log"; return 1
  fi
}

# crate absent from the resolved normal-dependency graph of a build config
tree_absent() { # <desc> <target> <features> <crate...>
  local desc=$1 target=$2 feats=$3; shift 3
  local c out
  for c in "$@"; do
    out=$(cargo tree --locked -e normal -i "$c" --target "$target" --features "$feats" 2>&1)
    if grep -q "did not match any packages" <<<"$out"; then
      ok "$desc: $c not in dependency graph"
    else
      bad "$desc: $c is in dependency graph"
    fi
  done
}

elf_probe() { # <desc> <elf> <expect: absent|present>
  local desc=$1 elf=$2 want=$3 n strs
  n=$(nm -C "$elf" 2>/dev/null | awk '$2 != "A" && $2 != "a"' | grep -ciE "$SYM_RE")
  strs=$(mktemp); strings -a "$elf" >"$strs"
  local nstr; nstr=$(grep -cE "$STR_RE" "$strs")
  local menu=0; grep -qx 'Upload' "$strs" && menu=1
  rm -f "$strs"
  if [ "$want" = absent ]; then
    [ "$n" -eq 0 ]    && ok "$desc: no radio/net symbols"       || bad "$desc: $n radio/net symbols"
    [ "$nstr" -eq 0 ] && ok "$desc: no upload-mode strings"     || bad "$desc: $nstr upload-mode strings"
    [ "$menu" -eq 0 ] && ok "$desc: no 'Upload' menu label"     || bad "$desc: 'Upload' menu label present"
  else
    [ "$n" -gt 0 ]    && ok "$desc: radio/net symbols found ($n)"      || bad "$desc: probe finds no radio symbols"
    [ "$nstr" -gt 0 ] && ok "$desc: upload-mode strings found ($nstr)" || bad "$desc: probe finds no upload strings"
    [ "$menu" -eq 1 ] && ok "$desc: 'Upload' menu label found"         || bad "$desc: no 'Upload' menu label"
  fi
}

mkdir -p "$root"

echo "== X4 offline (default)"
tree_absent "x4 offline" $X4T board-x4 "${RADIO_CRATES[@]}"
if build "$root/x4" build-x4; then
  elf_probe "x4 offline" "$root/x4/$X4T/release/pulp-os" absent
fi

echo "== X4 + wifi (positive control)"
tree_absent_wifi=$(cargo tree --locked -e normal -i esp-radio --target $X4T --features board-x4,wifi 2>&1)
grep -q '^esp-radio' <<<"$tree_absent_wifi" && ok "x4 wifi: esp-radio in dependency graph" \
  || bad "x4 wifi: esp-radio missing from graph"
if build "$root/x4-wifi" build-x4 --features wifi; then
  elf_probe "x4 wifi" "$root/x4-wifi/$X4T/release/pulp-os" present
fi

echo "== C61 (offline only)"
tree_absent "c61" $C61T board-onepage-c61 "${RADIO_CRATES[@]}"
if build "$root/c61" build-c61; then
  elf_probe "c61 full firmware" "$root/c61/$C61T/release/pulp-os-c61" absent
  elf_probe "c61 boot image" "$root/c61/$C61T/release/pulp-os-c61-boot" absent
fi

exit $fail
