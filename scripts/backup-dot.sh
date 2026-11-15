#!/usr/bin/env bash
# Back up the Echo Dot's eMMC before flashing Pinna. Read-only on the device;
# needs the running LibreEcho with adb.
#
#   scripts/backup-dot.sh ~/echo-backups/echodot2-$(date +%F)
#
# Writes into the (new) directory:
#   mmcblk0.img, mmcblk0boot{0,1}.img   the whole eMMC (~4 GB) and its boot areas
#   partitions.txt                      device, name, start and size (512-byte sectors)
#   device-info.txt                     boot area device numbers, kernel command line
#   partitions/*.img + SHA256SUMS       boot_a, boot_b, system_a, system_b, userdata,
#                                       misc: what `scripts/flash-dot.sh restore` takes
# Every partition except userdata (it changes while LibreEcho runs) is compared
# with the device before the partition images are cut.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
out=${1:?usage: $0 <backup dir>}
adb=${ADB:-adb}

command -v "$adb" >/dev/null || { echo "$adb not found (run inside nix develop)" >&2; exit 1; }
[ "$($adb get-state 2>/dev/null)" = device ] || { echo "no adb device (is LibreEcho running and USB connected?)" >&2; exit 1; }
[ ! -e "$out/mmcblk0.img" ] || { echo "$out already holds a backup" >&2; exit 1; }
mkdir -p "$out/partitions"
cd "$out"

$adb shell 'for p in /sys/block/mmcblk0/mmcblk0p*; do echo "$(basename $p) $(grep PARTNAME $p/uevent | cut -d= -f2) $(cat $p/start) $(cat $p/size)"; done' |
  tr -d '\r' > partitions.txt
$adb shell 'cat /sys/block/mmcblk0boot0/dev /sys/block/mmcblk0boot1/dev /proc/cmdline' |
  tr -d '\r' > device-info.txt

echo "==> whole eMMC (~4 GB)"
$adb pull /dev/mmcblk0 mmcblk0.img
for b in boot0 boot1; do
  mm=$($adb shell "cat /sys/block/mmcblk0$b/dev" | tr -d '\r')
  $adb shell "[ -e /tmp/$b ] || mknod /tmp/$b b ${mm%%:*} ${mm##*:}"
  $adb pull "/tmp/$b" "mmcblk0$b.img"
done

echo "==> comparing with the device"
"$root/scripts/verify-backup.sh" "$out"

echo "==> partition images"
while read -r _ name start size; do
  case $name in
    boot_a | boot_b | system_a | system_b | userdata | misc)
      dd if=mmcblk0.img of="partitions/$name.img" bs=512 skip="$start" count="$size" status=none
      ;;
  esac
done < partitions.txt
(cd partitions && sha256sum -- *.img > SHA256SUMS)
cat partitions/SHA256SUMS
echo "==> backup complete: $out (copy it somewhere safe before flashing)"
