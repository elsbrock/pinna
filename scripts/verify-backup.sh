#!/usr/bin/env bash
# Compare a backup's per-partition images with the live device (read-only).
# The device must still run the firmware the backup was taken from, with adb.
#
#   scripts/verify-backup.sh ~/echo-backups/echodot2-2026-11-15
#
# userdata is skipped: it changes while that firmware runs.
set -euo pipefail

backup=${1:?usage: $0 <backup dir>}
adb=${ADB:-adb}
parts=$backup/partitions.clean.txt
[ -s "$parts" ] || parts=$backup/partitions.txt
[ -s "$parts" ] || { echo "no partitions.txt in $backup" >&2; exit 1; }

[ "$($adb get-state 2>/dev/null)" = device ] || { echo "no adb device" >&2; exit 1; }

fail=0
while read -r dev name start size; do
  [ -n "$name" ] || continue
  [ "$name" = userdata ] && continue
  local_hash=$(dd if="$backup/mmcblk0.img" bs=512 skip="$start" count="$size" status=none | sha256sum | cut -d' ' -f1)
  device_hash=$($adb shell "sha256sum /dev/$dev" </dev/null | tr -d '\r' | cut -d' ' -f1)
  if [ "$local_hash" = "$device_hash" ]; then
    printf 'ok    %-10s %s\n' "$name" "${local_hash:0:16}"
  else
    printf 'DIFF  %-10s backup %s device %s\n' "$name" "${local_hash:0:16}" "${device_hash:0:16}"
    fail=1
  fi
done < <(tr -d '\r' < "$parts")
exit "$fail"
