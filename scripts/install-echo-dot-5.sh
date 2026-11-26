#!/usr/bin/env bash
# Install Pinna on an Echo Dot (5th gen, "cannoli", C2N6L4) that was unlocked
# with firebreak and runs TWRP (slot B), connected over adb (the test pads
# under the foot are its USB). ADR 0035, Decision 10.
# Also runs from a release folder, next to the images: ./install-echo-dot-5.sh check
#
#   scripts/install-echo-dot-5.sh check          # read-only: device, UBI volumes, artifacts
#   scripts/install-echo-dot-5.sh install [--yes] # boot_a <- FIT, system_a <- root, slot a on trial, reboot
#   scripts/install-echo-dot-5.sh restore        # says how to get Fire OS back
#
# Only the UBI volumes boot_a and system_a, and the slot record in misc, are
# ever written. Never touched: boot_b (TWRP), system_b, userdata, uboot_*,
# tee*, and every other volume. Each volume is replaced whole by
# `pinna-bootctl write-volume` in TWRP and read back against its sha256.
set -euo pipefail

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
die() { printf '\033[31merror: %s\033[0m\n' "$*" >&2; exit 1; }

for tool in adb sha256sum stat awk; do
  command -v "$tool" >/dev/null || die "$tool not found"
done

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/.." && pwd)
# In a release folder (the script next to the FIT) the artifacts are here; in
# the repository they are in build/ and target/.
if [ -s "$here/pinna-boot-mt8519.itb" ]; then
  fit=${FIT:-$here/pinna-boot-mt8519.itb}
  roots=("$here"/pinna-*.root.img)
  root_img=${ROOT_IMG:-${roots[0]}}
  [ -n "${ROOT_IMG:-}" ] || [ ${#roots[@]} -eq 1 ] || die "more than one pinna-*.root.img in $here; set ROOT_IMG"
  bootctl=${BOOTCTL:-$here/pinna-bootctl-armv7}
else
  fit=${FIT:-$root/build/pinna-boot-mt8519.itb}
  root_img=${ROOT_IMG:-$root/build/pinna.root.img}
  bootctl=${BOOTCTL:-$root/target/armv7-unknown-linux-musleabihf/release/pinna-bootctl}
fi

# What the volumes of a C2N6L4 take (crates/pinna-board, ECHO_DOT_5.slots).
boot_slot=$((0xC1C000))
root_slot=131088384
device_type_id=A4ZXE0RM7LQ7A

need_file() { [ -s "$1" ] || die "missing $1 ($2)"; }
size() { stat -c %s "$1"; }

# Run a command in TWRP; adb's output has CRs.
dev() { adb shell "$@" | tr -d '\r'; }

need_device() {
  local n
  # TWRP's adbd lists the unit as "recovery", not "device" (TWRP 3.7.0 on a
  # C2N6L4, 2026-11-26).
  n=$(adb devices | awk 'NR > 1 && ($2 == "device" || $2 == "recovery")' | wc -l)
  [ "$n" -eq 1 ] || die "adb sees $n devices, expected exactly 1 (hold the test pads' USB to the computer; TWRP in slot B must be running; set ANDROID_SERIAL to choose)"
  [ -n "$(dev getprop ro.twrp.version)" ] || die "the device is not running TWRP (ro.twrp.version is empty): boot slot B's TWRP"
  id=$(dev 'cat /proc/idme/device_type_id 2>/dev/null' | tr -d '\0')
  [ "$id" = "$device_type_id" ] || die "idme device_type_id is '${id:-?}', expected $device_type_id (Echo Dot 5th gen)"
}

# The UBI volumes as "node name data_bytes" lines.
volumes() {
  # shellcheck disable=SC2016 # expands in TWRP, not here
  dev 'for d in /sys/class/ubi/ubi*_*; do echo "$(basename $d) $(cat $d/name) $(cat $d/data_bytes)"; done'
}

# vol NAME: the node and size of a volume, or dies.
vol() {
  local line
  line=$(volumes | awk -v n="$1" '$2 == n {print $1, $3; exit}')
  [ -n "$line" ] || die "no UBI volume named $1 on the device"
  echo "$line"
}

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

# Refuse an image that can't boot or fit, before anything is written.
check_artifacts() {
  need_file "$fit" "just fit, or a release folder"
  need_file "$root_img" "just image, or a release folder"
  need_file "$bootctl" "cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p pinna-bootctl, or a release folder"
  [ "$(size "$fit")" -le "$boot_slot" ] || die "$fit is $(size "$fit") bytes, the boot slot takes $boot_slot"
  [ "$(size "$root_img")" -le "$root_slot" ] || die "$root_img is $(size "$root_img") bytes, the root slot takes $root_slot"
  check_sums "$fit" || say "no SHA256SUMS next to $(basename "$fit"): not checked"
  # Only a sealed slot image signed with the release key boots (pinna-init).
  if check_sums "$root_img"; then
    :
  elif [ -s "$root/Cargo.toml" ] && command -v cargo >/dev/null; then
    cargo run -q --release --manifest-path "$root/Cargo.toml" -p pinna-image -- verify "$root_img" ||
      die "$root_img is not a sealed slot image signed with the release key"
  else
    die "cannot check $root_img: no SHA256SUMS next to it, and no repository with cargo"
  fi
}

# write NAME FILE: stream FILE into the UBI volume NAME and check it read back.
write() {
  local sum bytes
  sum=$(sha256sum "$2" | cut -d' ' -f1)
  bytes=$(size "$2")
  say "$1 <- $(basename "$2") ($bytes bytes, sha256 $sum)"
  # `adb shell -T` streams stdin without a terminal and returns the command's
  # output and exit status. `adb exec-in` returns neither: on a C2N6L4 in
  # TWRP 3.7.0 (2026-11-26) both updates stopped part-way, left the volumes
  # flagged as interrupted, and the script carried on.
  adb shell -T "/tmp/pinna-bootctl write-volume $1 --size $bytes --sha256 $sum" < "$2"
}

case "${1:-}" in
  check)
    need_device
    say "device"
    printf '  %-28s %s\n' model "Echo Dot (5th gen)" twrp "$(dev getprop ro.twrp.version)" device_type_id "$id"
    say "UBI volumes (name, node, data bytes)"
    volumes | awk '{printf "  %-12s %-10s %s\n", $2, $1, $3}'
    for v in boot_a system_a misc; do vol "$v" >/dev/null; done
    say "artifacts"
    for f in "$fit" "$root_img" "$bootctl"; do
      if [ -s "$f" ]; then printf '  %s  %s (%s bytes)\n' "$(sha256sum "$f" | cut -c1-16)" "$f" "$(size "$f")"
      else printf '  MISSING %s\n' "$f"; fi
    done
    [ ! -s "$fit" ] || printf '  boot_a takes %s, the FIT is %s\n' "$boot_slot" "$(size "$fit")"
    [ ! -s "$root_img" ] || printf '  system_a takes %s, the root is %s\n' "$root_slot" "$(size "$root_img")"
    ;;
  install)
    yes=0
    [ "${2:-}" != --yes ] || yes=1
    need_device
    check_artifacts
    read -r boot_node boot_cap <<<"$(vol boot_a)"
    read -r sys_node sys_cap <<<"$(vol system_a)"
    read -r misc_node _ <<<"$(vol misc)"
    [ -n "$boot_node" ] && [ -n "$sys_node" ] && [ -n "$misc_node" ] || die "boot_a, system_a or misc is missing"
    [ "$(size "$fit")" -le "$boot_cap" ] || die "the FIT does not fit boot_a ($boot_cap bytes)"
    [ "$(size "$root_img")" -le "$sys_cap" ] || die "the root does not fit system_a ($sys_cap bytes)"
    say "this writes, in TWRP over adb:"
    say "boot_a   (/dev/$boot_node) <- $(basename "$fit") ($(size "$fit") bytes)"
    say "system_a (/dev/$sys_node) <- $(basename "$root_img") ($(size "$root_img") bytes)"
    say "misc     (/dev/$misc_node) <- slot a active, on trial (the slot record only)"
    say "boot_b (TWRP), system_b, userdata and every other volume stay as they are."
    say "A slot a that fails its boots falls back to TWRP in slot B. The way back to Fire OS is firebreak."
    if [ "$yes" -ne 1 ]; then
      # End of input (a pipe that adb already read) is a no, said out loud.
      read -r -p 'Type "yes" to write: ' a || a=
      [ "$a" = yes ] || die "aborted"
    fi
    say "pushing pinna-bootctl to /tmp in TWRP"
    adb push "$bootctl" /tmp/pinna-bootctl >/dev/null
    dev chmod 755 /tmp/pinna-bootctl
    [ "$(dev /tmp/pinna-bootctl storage-kind)" = ubi ] || die "pinna-bootctl in TWRP does not see UBI storage"
    [ "$(dev /tmp/pinna-bootctl bootloader)" = amazon-uboot ] || die "pinna-bootctl in TWRP does not see Amazon's U-Boot"
    write boot_a "$fit"
    write system_a "$root_img"
    say "slot a active, on trial"
    adb shell -T "/tmp/pinna-bootctl --misc /dev/$misc_node set-active a"
    say "rebooting"
    adb reboot
    say "done: the satellite starts Pinna from slot a; its ring shows the boot stages"
    ;;
  restore)
    say "Fire OS is not restored from here: this script has no Fire OS images."
    say "Restore it with firebreak. While slot B is still TWRP, holding Volume Up at power-on starts it."
    ;;
  *)
    sed -n '2,12p' "$0"
    exit 2
    ;;
esac
