#!/usr/bin/env bash
# Rescue mode under QEMU (ADR 0012). pinna-init boots two disks it cannot
# boot from and must stay up in rescue mode instead of rebooting:
#
#   no-root     misc, expdb, system_a, system_b, userdata, but no image in
#               either slot
#   no-storage  misc and userdata only: no slot partitions (15 s wait)
#
# Checked on the serial console: the rescue line with its reason, the rescue
# shell's banner, no second boot within STAY seconds. QEMU has no USB device
# controller, so the gadget, DHCP and the page are left to the device.
#
#   tests/vm/rescue-test.sh                      # armhf: QEMU virt (TCG)
#   ARCH=x86_64 tests/vm/rescue-test.sh          # x86_64: KVM
#   KERNEL=... INITRD=... tests/vm/rescue-test.sh
#
# Needs: just kernel-qemu initramfs (or kernel-x86 initramfs-x86); sgdisk.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck source=lib.sh
source "$root/tests/vm/lib.sh"
arch=${ARCH:-armhf}
case "$arch" in
  armhf)
    kernel=${KERNEL:-$root/build/kernel-qemu/arch/arm/boot/zImage}
    initrd=${INITRD:-$root/build/initramfs.cpio} ;;
  x86_64)
    kernel=${KERNEL:-$root/build/kernel-x86/arch/x86/boot/bzImage}
    initrd=${INITRD:-$root/build/initramfs-x86.cpio} ;;
  *) echo "unknown ARCH=$arch" >&2; exit 2 ;;
esac
work=${WORK:-$root/build/vm-rescue-$arch}
stay=${STAY:-30}
mkdir -p "$work"

cleanup() { [ -f "$work/qemu.pid" ] && kill "$(cat "$work/qemu.pid")" 2>/dev/null || true; }
trap cleanup EXIT

# The device's layout with empty slots.
no_root_disk() {
  local empty=$work/empty.img
  truncate -s 1M "$empty"
  make_disk "$1" "$empty"
}

no_storage_disk() {
  rm -f "$1"
  truncate -s 600M "$1"
  sgdisk -a 2048 -n 1:0:+512K -c 1:misc -n 2:0:+512M -c 2:userdata "$1" >/dev/null
}

check_case() { # name reason
  local name=$1 reason=$2 disk=$work/$1.raw console=$work/$1.console.log
  log "$name: booting a disk pinna-init cannot boot from"
  rm -f "$console" "$work/qemu.pid"
  qemu_start "$arch" "$kernel" "$initrd" "$disk" "$console" "$work/qemu.pid" "user,id=n0"
  for _ in $(seq 1 60); do
    console_text "$console" | grep -q "pinna-init: rescue mode ($reason)" && break
    sleep 2
  done
  if console_text "$console" | grep -q "pinna-init: rescue mode ($reason)"; then
    pass "$name: rescue mode ($reason)"
  else
    fail "$name: no 'rescue mode ($reason)' on the console"
  fi
  sleep "$stay"
  if console_text "$console" | grep -q "Pinna rescue mode ($reason)"; then
    pass "$name: rescue shell on the console"
  else
    fail "$name: no rescue shell banner on the console"
  fi
  local boots
  boots=$(console_text "$console" | grep -c "Run /init as init process" || true)
  if [ "$boots" = 1 ]; then
    pass "$name: still up after ${stay}s, no reboot"
  else
    fail "$name: booted $boots times"
  fi
  kill "$(cat "$work/qemu.pid")" 2>/dev/null || true
  sleep 1
}

no_root_disk "$work/no-root.raw"
check_case no-root no-root
no_storage_disk "$work/no-storage.raw"
check_case no-storage no-storage

if [ "$status" = 0 ]; then log "VM rescue gate PASSED"; else log "VM rescue gate FAILED (consoles in $work)"; fi
exit "$status"
