#!/usr/bin/env bash
# Boot the real Pinna image (mkosi/mkosi.output/pinna.raw: misc + system_a +
# userdata) under QEMU `virt` with the Pinna kernel's QEMU variant and check
# that it comes up healthy:
#
#  - from the host, while it runs: pinnad answers Home Assistant's ESPHome client;
#  - from the VM's own journal (read off its userdata partition after power-off):
#    erofs root, /var formatted and mounted, multi-user reached, no failed units,
#    pinnad started and published mDNS, boot marked healthy.
#
#   tests/vm/boot-test.sh                  # boot, check, power off
#   SSH_KEY=~/.ssh/key tests/vm/boot-test.sh   # also run checks over ssh
#   KEEP=1 tests/vm/boot-test.sh           # leave the VM running
#
# Needs: just kernel-qemu image; sgdisk, debugfs, journalctl on the host.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=${WORK:-$root/build/vm}
kernel=$root/build/kernel-qemu/arch/arm/boot/zImage
image=$root/mkosi/mkosi.output/pinna.raw
ssh_port=${SSH_PORT:-2222}
api_port=${API_PORT:-16053}
timeout_s=${BOOT_TIMEOUT:-600}

mkdir -p "$work"
cp --sparse=always "$image" "$work/disk.raw"
rm -f "$work/console.log"

log() { printf '\033[1m==> %s\033[0m\n' "$*"; }
status=0
fail() { printf '\033[31mFAIL: %s\033[0m\n' "$*"; status=1; }
pass() { printf '\033[32mok\033[0m   %s\n' "$*"; }

log "booting $(basename "$image") under QEMU (TCG; this takes a few minutes)"
qemu-system-arm \
  -M virt,highmem=off -cpu max -smp 2 -m 512 \
  -kernel "$kernel" \
  -drive if=none,file="$work/disk.raw",format=raw,id=hd0 \
  -device virtio-blk-device,drive=hd0 \
  -netdev user,id=n0,hostfwd=tcp:127.0.0.1:${ssh_port}-:22,hostfwd=tcp:127.0.0.1:${api_port}-:6053 \
  -device virtio-net-device,netdev=n0 \
  -device virtio-rng-device \
  -display none -serial file:"$work/console.log" \
  -pidfile "$work/qemu.pid" -daemonize

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
range=$(sgdisk -i 3 "$work/disk.raw" | awk '/First sector/{f=$3} /Last sector/{l=$3} END{print f, l}')
read -r first last <<<"$range"
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
jcheck "boot marked healthy" "Finished pinna-boot-ok.service"
failed=$(grep -E "failed with result|Failed to start" "$work/journal.txt" || true)
if [ -z "$failed" ]; then pass "no failed units"; else fail "failed units:"; echo "$failed" | sed 's/^/     /'; fi
grep -m1 "Startup finished" "$work/journal.txt" | sed 's/^/     /' || true

if [ "$status" = 0 ]; then log "VM boot gate PASSED"; else log "VM boot gate FAILED (console: $work/console.log)"; fi
exit "$status"
