#!/usr/bin/env bash
# Boot a real Pinna image (GPT disk: misc + expdb + system_a + userdata) under
# QEMU with a Pinna kernel variant and check that it comes up healthy:
#
#  - from the host, while it runs: pinnad answers Home Assistant's ESPHome client;
#  - from the VM's own journal (read off its userdata partition after power-off):
#    erofs root, /var formatted and mounted, multi-user reached, no failed units,
#    pinnad started and published mDNS, fastboot guard armed and cleared, boot
#    marked healthy; and misc's bootloader command is empty again.
#
#   tests/vm/boot-test.sh                      # armhf: mkosi.output/pinna.raw, QEMU virt (TCG)
#   ARCH=x86_64 tests/vm/boot-test.sh          # x86_64: mkosi.output/pinna-x86.raw, KVM
#   SSH_KEY=~/.ssh/key tests/vm/boot-test.sh   # also run checks over ssh
#   KEEP=1 tests/vm/boot-test.sh               # leave the VM running
#
# Needs: just kernel-qemu image (or kernel-x86 image-x86); sgdisk, debugfs,
# journalctl on the host.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
arch=${ARCH:-armhf}
case "$arch" in
  armhf)
    work=${WORK:-$root/build/vm}
    kernel=$root/build/kernel-qemu/arch/arm/boot/zImage
    image=$root/mkosi/mkosi.output/pinna.raw ;;
  x86_64)
    work=${WORK:-$root/build/vm-x86}
    kernel=$root/build/kernel-x86/arch/x86/boot/bzImage
    image=$root/mkosi/mkosi.output/pinna-x86.raw ;;
  *) echo "unknown ARCH=$arch" >&2; exit 2 ;;
esac
ssh_port=${SSH_PORT:-2222}
api_port=${API_PORT:-16053}
timeout_s=${BOOT_TIMEOUT:-600}

mkdir -p "$work"
cp --sparse=always "$image" "$work/disk.raw"
rm -f "$work/console.log"

# Partition by GPT name -> "first last" sector.
part_range() {
  local n
  n=$(sgdisk -p "$work/disk.raw" | awk -v name="$1" '$NF==name {print $1}')
  [ -n "$n" ] || { echo "no partition $1" >&2; return 1; }
  sgdisk -i "$n" "$work/disk.raw" | awk '/First sector/{f=$3} /Last sector/{l=$3} END{print f, l}'
}

# A fake Kaeru LK payload header in expdb, so the fastboot guard arms like on
# the device (MTK image magic, then the image name "LK").
read -r expdb_first _ <<<"$(part_range expdb)"
printf '\x88\x16\x88\x58\x00\x00\x00\x00LK\x00' |
  dd of="$work/disk.raw" bs=512 seek="$expdb_first" conv=notrunc status=none

log() { printf '\033[1m==> %s\033[0m\n' "$*"; }
status=0
fail() { printf '\033[31mFAIL: %s\033[0m\n' "$*"; status=1; }
pass() { printf '\033[32mok\033[0m   %s\n' "$*"; }

netdev="user,id=n0,hostfwd=tcp:127.0.0.1:${ssh_port}-:22,hostfwd=tcp:127.0.0.1:${api_port}-:6053"
log "booting $(basename "$image") ($arch) under QEMU"
if [ "$arch" = armhf ]; then
  qemu-system-arm \
    -M virt,highmem=off -cpu max -smp 2 -m 512 \
    -kernel "$kernel" \
    -drive if=none,file="$work/disk.raw",format=raw,id=hd0 \
    -device virtio-blk-device,drive=hd0 \
    -netdev "$netdev" -device virtio-net-device,netdev=n0 \
    -device virtio-rng-device \
    -display none -serial file:"$work/console.log" \
    -pidfile "$work/qemu.pid" -daemonize
else
  accel=tcg
  [ -w /dev/kvm ] && accel=kvm
  qemu-system-x86_64 \
    -M q35,accel="$accel" -cpu max -smp 2 -m 1024 \
    -kernel "$kernel" \
    -drive if=none,file="$work/disk.raw",format=raw,id=hd0 \
    -device virtio-blk-pci,drive=hd0 \
    -netdev "$netdev" -device virtio-net-pci,netdev=n0 \
    -device virtio-rng-pci \
    -display none -serial file:"$work/console.log" \
    -pidfile "$work/qemu.pid" -daemonize
fi

cleanup() {
  if [ -z "${KEEP:-}" ] && [ -f "$work/qemu.pid" ]; then
    kill "$(cat "$work/qemu.pid")" 2>/dev/null || true
  fi
}
trap cleanup EXIT

console() { sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' "$work/console.log" 2>/dev/null; }

log "waiting for pinnad's ESPHome API (boot takes a few minutes under TCG)"
for _ in $(seq 1 $((timeout_s / 5))); do
  (exec 3<>/dev/tcp/127.0.0.1/"$api_port") 2>/dev/null && break
  sleep 5
done
# Let late units (pinna-boot-ok waits 10 s) finish and the journal reach the
# virtual disk (the VM is killed, not shut down, so allow for writeback).
sleep 90

log "ESPHome API via aioesphomeapi"
if uv run -q --with aioesphomeapi==46.2.0 python - "$api_port" <<'EOF'
import asyncio, sys
from aioesphomeapi import APIClient

async def main(port):
    c = APIClient("127.0.0.1", port, None)
    for _ in range(30):
        try:
            await c.connect(login=True)
            break
        except Exception:
            await asyncio.sleep(2)
    info = await c.device_info()
    ents, _ = await c.list_entities_services()
    kinds = sorted(type(e).__name__ for e in ents)
    print(f"     device {info.name} ({info.friendly_name}) mac={info.mac_address} "
          f"esphome={info.esphome_version} voice_flags={info.voice_assistant_feature_flags} entities={kinds}")
    assert info.voice_assistant_feature_flags == 61
    assert {"LightInfo", "SwitchInfo"} <= set(kinds)
    await c.disconnect()

asyncio.run(main(int(sys.argv[1])))
EOF
then pass "pinnad answers the ESPHome API"; else fail "ESPHome API"; fi

if [ -n "${SSH_KEY:-}" ]; then
  log "ssh checks"
  ssh_vm() {
    ssh -p "$ssh_port" -i "$SSH_KEY" -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o LogLevel=ERROR root@127.0.0.1 "$@"
  }
  ssh_vm 'systemctl is-system-running; systemctl --failed --no-legend; findmnt -no SOURCE,FSTYPE / /var; ls /run/systemd/dnssd' | sed 's/^/     /' || fail "ssh"
fi

log "stopping the VM and reading its journal from the userdata partition"
kill "$(cat "$work/qemu.pid")" 2>/dev/null || true
sleep 2
read -r first last <<<"$(part_range userdata)"
dd if="$work/disk.raw" of="$work/userdata.ext4" bs=512 skip="$first" count=$((last - first + 1)) status=none
# The VM was killed, not shut down: replay ext4's own journal on the copy.
e2fsck -fy "$work/userdata.ext4" >/dev/null 2>&1 || true
rm -rf "${work:?}/journal"
mkdir -p "$work/journal"
debugfs -c -R "rdump /log/journal $work/journal" "$work/userdata.ext4" >/dev/null 2>&1 || true
journalctl --directory="$work/journal/journal" --no-pager -o cat > "$work/journal.txt" 2>/dev/null || true
jcheck() { if grep -qE "$2" "$work/journal.txt"; then pass "$1"; else fail "$1"; fi; }
jcheck "kernel mounted the erofs root" "Mounted root \(erofs"
jcheck "/var formatted on first boot" "successfully formatted as ext4"
jcheck "/var mounted" "Mounted var.mount"
jcheck "multi-user.target reached" "Reached target multi-user.target"
jcheck "pinnad started" "Started pinnad.service"
jcheck "mDNS service published" "published _esphomelib._tcp"
jcheck "fastboot guard armed" "armed: next boot enters fastboot unless cleared"
jcheck "fastboot guard cleared by the health check" "cleared: boot marked healthy"
jcheck "boot marked healthy" "Finished pinna-boot-ok.service"
read -r misc_first _ <<<"$(part_range misc)"
if [ -z "$(dd if="$work/disk.raw" bs=512 skip="$misc_first" count=1 status=none | head -c 32 | tr -d '\0')" ]; then
  pass "misc bootloader command empty after boot"
else
  fail "misc bootloader command left set: $(dd if="$work/disk.raw" bs=512 skip="$misc_first" count=1 status=none | head -c 32 | tr -d '\0')"
fi
failed=$(grep -E "failed with result|Failed to start" "$work/journal.txt" || true)
if [ -z "$failed" ]; then pass "no failed units"; else fail "failed units:"; echo "$failed" | sed 's/^/     /'; fi
grep -m1 "Startup finished" "$work/journal.txt" | sed 's/^/     /' || true

if [ "$status" = 0 ]; then log "VM boot gate PASSED"; else log "VM boot gate FAILED (console: $work/console.log)"; fi
exit "$status"
