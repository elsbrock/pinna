#!/usr/bin/env bash
# Flash Pinna onto an Echo (2nd gen, "radar") or an Echo Dot (2nd gen,
# "biscuit") that is in fastboot mode (hold Volume Down while plugging the
# device in).
# Also runs from a release folder, next to the images: ./flash-gen2.sh root
#
#   scripts/flash-gen2.sh check        # read-only: device, partition sizes, artifacts
#   scripts/flash-gen2.sh root         # system_a <- sealed slot image, clear system_b, wipe userdata
#   scripts/flash-gen2.sh try          # RAM-boot the Pinna kernel once (nothing written)
#   scripts/flash-gen2.sh boot         # boot_a + boot_b <- Pinna boot image (persistent)
#   scripts/flash-gen2.sh restore DIR  # put the previous firmware back from a backup's partitions/
#
# Only boot_a, boot_b, system_a, system_b and userdata are ever written. Never
# touched: preloader/boot areas, lk_a/lk_b, tee1/tee2, expdb (Kaeru), persist,
# misc (Pinna writes only its bootloader command and A/B slot state, at runtime).
set -euo pipefail

fb=${FASTBOOT:-fastboot}

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
die() { printf '\033[31merror: %s\033[0m\n' "$*" >&2; exit 1; }
confirm() {
  read -r -p "$1 [y/N] " a
  [ "$a" = y ] || [ "$a" = Y ] || die "aborted"
}

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/.." && pwd)
# In a release folder (the script next to pinna-boot.img) the images are
# here; in the repository they are in build/.
if [ -s "$here/pinna-boot.img" ]; then
  boot_img=${BOOT_IMG:-$here/pinna-boot.img}
  roots=("$here"/pinna-*.root.img)
  root_img=${ROOT_IMG:-${roots[0]}}
  [ -n "${ROOT_IMG:-}" ] || [ ${#roots[@]} -eq 1 ] || die "more than one pinna-*.root.img in $here; set ROOT_IMG"
else
  boot_img=${BOOT_IMG:-$root/build/pinna-boot.img}
  root_img=${ROOT_IMG:-$root/build/pinna.root.img}
fi

var() { $fb getvar "$1" 2>&1 | awk -F': ' -v k="$1" '$1==k {print $2; exit}' | tr -d '\r'; }

need_device() {
  [ -n "$($fb devices)" ] || die "no fastboot device (hold Volume Down while plugging the device in)"
  local product
  product=$(var product)
  case "$product" in
    BISCUIT) model="Echo Dot (2nd gen)" ;;
    RADAR)   model="Echo (2nd gen)" ;;
    *) die "fastboot product is '${product:-?}', expected BISCUIT (Echo Dot 2nd gen) or RADAR (Echo 2nd gen)" ;;
  esac
}

need_file() { [ -s "$1" ] || die "missing $1 ($2)"; }

# 0 if FILE is listed in the SHA256SUMS next to it and matches, 1 if there
# is no SHA256SUMS; dies if it isn't listed or doesn't match.
check_sums() {
  local dir name line
  dir=$(dirname "$1"); name=$(basename "$1")
  [ -s "$dir/SHA256SUMS" ] || return 1
  line=$(awk -v n="$name" '$2 == n || $2 == "*" n' "$dir/SHA256SUMS")
  [ -n "$line" ] || die "$name is not listed in $dir/SHA256SUMS"
  (cd "$dir" && printf '%s\n' "$line" | sha256sum -c --status -) || die "$name does not match $dir/SHA256SUMS"
  say "$name matches SHA256SUMS"
}

case "${1:-}" in
  check)
    need_device
    say "device"
    printf '  %-28s %s\n' model "$model"
    for v in product serialno unlocked max-download-size partition-size:boot_a partition-size:system_a partition-size:system_b partition-size:userdata; do
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
    need_file "$root_img" "just image, or a release folder"
    # Only a sealed slot image signed with the release key boots (pinna-init).
    # Android sparse images (img2simg) cannot be checked here.
    if check_sums "$root_img"; then
      :
    elif [ "$(od -An -tx4 -N4 "$root_img" | tr -d ' ')" = ed26ff3a ]; then
      say "$(basename "$root_img") is a sparse image: signature not checked"
    elif [ -s "$root/Cargo.toml" ] && command -v cargo >/dev/null; then
      cargo run -q --release --manifest-path "$root/Cargo.toml" -p pinna-image -- verify "$root_img" ||
        die "$root_img is not a sealed slot image signed with the release key"
    else
      die "cannot check $root_img: no SHA256SUMS next to it, and no repository with cargo"
    fi
    say "system_a <- $(basename "$root_img") ($(stat -c %s "$root_img") bytes)"
    say "system_b <- cleared (first MiB zeroed: no image, so slot a boots)"
    say "userdata <- wiped (Pinna formats it as /var on first boot)"
    confirm "This replaces system_a, clears system_b and erases userdata. Continue?"
    $fb flash system_a "$root_img"
    tmp=$(mktemp)
    head -c 1048576 /dev/zero > "$tmp"
    $fb flash system_b "$tmp"
    head -c 4194304 /dev/zero > "$tmp"
    $fb flash userdata "$tmp"
    rm -f "$tmp"
    say "done; next: $0 boot"
    ;;
  try)
    need_device
    need_file "$boot_img" "just bootimg, or a release folder"
    check_sums "$boot_img" || say "no SHA256SUMS next to $(basename "$boot_img"): not checked"
    say "RAM-booting $(basename "$boot_img") (nothing is written; a power cycle returns to the flashed kernel)"
    $fb boot "$boot_img"
    say "watch for the USB serial console (/dev/ttyACM*) and network (ssh root@10.42.0.1)"
    ;;
  boot)
    need_device
    need_file "$boot_img" "just bootimg, or a release folder"
    check_sums "$boot_img" || say "no SHA256SUMS next to $(basename "$boot_img"): not checked"
    confirm "Write the Pinna boot image to boot_a and boot_b?"
    $fb flash boot_a "$boot_img"
    $fb flash boot_b "$boot_img"
    $fb reboot
    ;;
  restore)
    dir=${2:?usage: $0 restore <backup>/partitions}
    need_device
    for p in boot_a boot_b system_a userdata; do need_file "$dir/$p.img" "backup partition"; done
    (cd "$dir" && sha256sum -c --ignore-missing SHA256SUMS) || die "backup checksums do not match"
    confirm "Restore the backup (boot_a, boot_b, system_a, system_b, userdata) from $dir?"
    $fb flash boot_a "$dir/boot_a.img"
    $fb flash boot_b "$dir/boot_b.img"
    $fb flash system_a "$dir/system_a.img"
    if [ -s "$dir/system_b.img" ]; then
      $fb flash system_b "$dir/system_b.img"
    else
      say "no system_b.img in the backup: system_b keeps Pinna's data"
    fi
    $fb flash userdata "$dir/userdata.img"
    $fb reboot
    ;;
  *)
    sed -n '2,15p' "$0"
    exit 2
    ;;
esac
