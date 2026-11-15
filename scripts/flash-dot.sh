#!/usr/bin/env bash
# Flash Pinna onto an Echo Dot (2nd gen, "biscuit") that is in fastboot mode
# (hold Volume Down while plugging in the micro-USB cable).
#
#   scripts/flash-dot.sh check        # read-only: device, partition sizes, artifacts
#   scripts/flash-dot.sh root         # system_a <- erofs root, wipe userdata (Pinna's /var)
#   scripts/flash-dot.sh try          # RAM-boot the Pinna kernel once (nothing written)
#   scripts/flash-dot.sh boot         # boot_a + boot_b <- Pinna boot image (persistent)
#   scripts/flash-dot.sh restore DIR  # put LibreEcho back from a backup's partitions/
#
# Only boot_a, boot_b, system_a and userdata are ever written. Never touched:
# preloader/boot areas, lk_a/lk_b, tee1/tee2, expdb (Kaeru), persist, misc.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
boot_img=${BOOT_IMG:-$root/build/pinna-boot.img}
root_img=${ROOT_IMG:-$root/mkosi/mkosi.output/pinna.root-arm.raw}
fb=${FASTBOOT:-fastboot}

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
die() { printf '\033[31merror: %s\033[0m\n' "$*" >&2; exit 1; }
confirm() {
  read -r -p "$1 [y/N] " a
  [ "$a" = y ] || [ "$a" = Y ] || die "aborted"
}

var() { $fb getvar "$1" 2>&1 | awk -F': ' -v k="$1" '$1==k {print $2; exit}' | tr -d '\r'; }

need_device() {
  [ -n "$($fb devices)" ] || die "no fastboot device (hold Volume Down while plugging in)"
  local product
  product=$(var product)
  [ "$product" = BISCUIT ] || die "fastboot product is '${product:-?}', expected BISCUIT (Echo Dot 2nd gen)"
}

need_file() { [ -s "$1" ] || die "missing $1 ($2)"; }

case "${1:-}" in
  check)
    need_device
    say "device"
    for v in product serialno unlocked max-download-size partition-size:boot_a partition-size:system_a partition-size:userdata; do
      printf '  %-28s %s\n' "$v" "$(var "$v")"
    done
    say "artifacts"
    for f in "$boot_img" "$root_img"; do
      if [ -s "$f" ]; then printf '  %s  %s (%s bytes)\n' "$(sha256sum "$f" | cut -c1-16)" "$f" "$(stat -c %s "$f")"
      else printf '  MISSING %s\n' "$f"; fi
    done
    ;;
  root)
    need_device
    need_file "$root_img" "just image"
    say "system_a <- $(basename "$root_img") ($(stat -c %s "$root_img") bytes)"
    say "userdata <- wiped (Pinna formats it as /var on first boot)"
    confirm "This replaces LibreEcho's system_a and erases userdata. Continue?"
    $fb flash system_a "$root_img"
    tmp=$(mktemp)
    head -c 4194304 /dev/zero > "$tmp"
    $fb flash userdata "$tmp"
    rm -f "$tmp"
    say "done; next: $0 try"
    ;;
  try)
    need_device
    need_file "$boot_img" "just bootimg"
    say "RAM-booting $(basename "$boot_img") (nothing is written; a power cycle returns to the flashed kernel)"
    $fb boot "$boot_img"
    say "watch for the USB serial console (/dev/ttyACM*) and network (ssh root@10.42.0.1)"
    ;;
  boot)
    need_device
    need_file "$boot_img" "just bootimg"
    confirm "Write the Pinna boot image to boot_a and boot_b?"
    $fb flash boot_a "$boot_img"
    $fb flash boot_b "$boot_img"
    $fb reboot
    ;;
  restore)
    dir=${2:?usage: $0 restore <backup>/partitions}
    need_device
    for p in boot_a system_a userdata; do need_file "$dir/$p.img" "backup partition"; done
    (cd "$dir" && sha256sum -c --ignore-missing SHA256SUMS) || die "backup checksums do not match"
    confirm "Restore LibreEcho (boot_a, boot_b, system_a, userdata) from $dir?"
    $fb flash boot_a "$dir/boot_a.img"
    $fb flash boot_b "$dir/boot_b.img"
    $fb flash system_a "$dir/system_a.img"
    $fb flash userdata "$dir/userdata.img"
    $fb reboot
    ;;
  *)
    sed -n '2,13p' "$0"
    exit 2
    ;;
esac
