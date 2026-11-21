#!/usr/bin/env bash
# Boot a real Pinna image under QEMU with a Pinna kernel variant and the
# pinna-init initramfs, from a test disk laid out like the device (misc, expdb,
# system_a = the sealed slot image, system_b empty, userdata), and check that it
# comes up healthy:
#
#  - from the host, while it runs: pinnad answers Home Assistant's ESPHome client;
#    on x86_64, the virtual Echo Dot (ADR 0014) works through its socket;
#  - from the VM's own journal (read off its userdata partition after power-off):
#    pinna-init chose slot a, verified it and booted it through dm-verity (dm-0),
#    /var formatted and mounted, multi-user reached, no failed units, pinnad
#    started and published mDNS, fastboot guard armed and cleared, slot a marked
#    good by the health check;
#  - from misc: bootloader command empty again, slot state "active a, a good".
#
#   tests/vm/boot-test.sh                      # armhf: build/pinna.root.img, QEMU virt (TCG)
#   ARCH=x86_64 tests/vm/boot-test.sh          # x86_64: build/pinna-x86.root.img, KVM
#   ARCH=arm64 tests/vm/boot-test.sh           # armhf image on the arm64 kernel (ADR 0031), TCG
#   SSH_KEY=~/.ssh/key tests/vm/boot-test.sh   # also run checks over ssh
#   KEEP=1 tests/vm/boot-test.sh               # leave the VM running
#
# Needs: just kernel-qemu image initramfs (kernel-qemu-arm64 image initramfs
# for arm64, kernel-x86 image-x86 initramfs-x86 for x86_64); sgdisk, debugfs,
# journalctl on the host.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck source=lib.sh
source "$root/tests/vm/lib.sh"
arch=${ARCH:-armhf}
case "$arch" in
  armhf)
    work=${WORK:-$root/build/vm}
    kernel=$root/build/kernel-qemu/arch/arm/boot/zImage
    initrd=$root/build/initramfs.cpio
    image=$root/build/pinna.root.img ;;
  arm64)
    work=${WORK:-$root/build/vm-arm64}
    kernel=$root/build/kernel-qemu-arm64/arch/arm64/boot/Image
    initrd=$root/build/initramfs.cpio
    image=$root/build/pinna.root.img ;;
  x86_64)
    work=${WORK:-$root/build/vm-x86}
    kernel=$root/build/kernel-x86/arch/x86/boot/bzImage
    initrd=$root/build/initramfs-x86.cpio
    image=$root/build/pinna-x86.root.img ;;
  *) echo "unknown ARCH=$arch" >&2; exit 2 ;;
esac
ssh_port=${SSH_PORT:-2222}
api_port=${API_PORT:-16053}
web_port=${WEB_PORT:-18080}
timeout_s=${BOOT_TIMEOUT:-600}

mkdir -p "$work"
version=$(cargo run -q --manifest-path "$root/Cargo.toml" -p pinna-image -- info "$image" | sed -n 's/^version: //p')
make_disk "$work/disk.raw" "$image"
rm -f "$work/console.log"

netdev="user,id=n0,hostfwd=tcp:127.0.0.1:${ssh_port}-:22,hostfwd=tcp:127.0.0.1:${api_port}-:6053,hostfwd=tcp:127.0.0.1:${web_port}-:80"
log "booting $(basename "$image") (version $version, $arch) under QEMU"
qemu_start "$arch" "$kernel" "$initrd" "$work/disk.raw" "$work/console.log" "$work/qemu.pid" "$netdev"

cleanup() {
  if [ -z "${KEEP:-}" ] && [ -f "$work/qemu.pid" ]; then
    kill "$(cat "$work/qemu.pid")" 2>/dev/null || true
  fi
}
trap cleanup EXIT

log "waiting for pinnad's ESPHome API (boot takes a few minutes under TCG)"
for _ in $(seq 1 $((timeout_s / 5))); do
  (exec 3<>/dev/tcp/127.0.0.1/"$api_port") 2>/dev/null && break
  sleep 5
done
# Let late units (pinna-boot-ok waits 10 s) finish and the journal reach the
# virtual disk (the VM is killed, not shut down, so allow for writeback).
sleep 90

log "ESPHome API via aioesphomeapi"
# Owner device images carry the Home Assistant API key
# (secrets/device/esphome-key); the x86_64 image does not (ADR 0014).
api_key=""
[ "$arch" != x86_64 ] && [ -s "$root/secrets/device/esphome-key" ] && api_key=$(tr -d '\n' <"$root/secrets/device/esphome-key")
if uv run -q --with aioesphomeapi==46.2.0 python - "$api_port" "$api_key" <<'EOF'
import asyncio, sys
from aioesphomeapi import APIClient

async def main(port, key):
    c = APIClient("127.0.0.1", port, None, noise_psk=key or None)
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

asyncio.run(main(int(sys.argv[1]), sys.argv[2] if len(sys.argv) > 2 else ""))
EOF
then pass "pinnad answers the ESPHome API"; else fail "ESPHome API"; fi

if [ "$arch" = x86_64 ]; then
  log "virtual Echo Dot (ADR 0014): buttons, microphone switch, speaker and microphone over /virtual-device"
  wake=()
  [ -d "$root/blobs/models" ] || { wake=(--no-wake-word); log "no wake word models (just models): skipping the wake word"; }
  if uv run -q --with aioesphomeapi==46.2.0 --with websockets==15.0.1 python "$root/tests/vm/virtual_dot.py" \
       "$api_port" "$web_port" "$root/crates/pinna-wakeword/tests/fixtures/alexa-twice.wav" "${wake[@]}" | sed 's/^/  /'; then
    pass "virtual Echo Dot"
  else
    fail "virtual Echo Dot"
  fi
fi

log "dashboard and control API on the LAN listener (port 80)"
web="http://127.0.0.1:${web_port}"
if curl -fsS --max-time 10 "$web/" | grep -q '<div id="app">'; then
  pass "dashboard served on port 80"
else
  fail "dashboard on port 80"
fi
if curl -fsS --max-time 10 -H 'Content-Type: application/json' -d '{}' \
     "$web/pinna.v1.DeviceService/GetStatus" | grep -q '"status"'; then
  pass "GetStatus over the web listener"
else
  fail "GetStatus over the web listener"
fi
code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 -H 'Host: evil.example' "$web/")
if [ "$code" = 421 ]; then pass "foreign Host refused (421)"; else fail "foreign Host got $code, not 421"; fi
# ADR 0020: host metrics from /proc, a service's cgroup, and pinnad's own.
metrics=$(curl -fsS --max-time 10 "$web/metrics" || true)
missing=""
for m in '^node_memory_MemAvailable_bytes ' '^node_cpu_seconds_total\{cpu="0",mode="idle"\} ' \
         '^pinna_unit_memory_bytes\{unit="pinnad.service"\} ' '^pinna_build_info\{' \
         '^pinna_home_assistant_connected '; do
  grep -Eq "$m" <<<"$metrics" || missing="$missing $m"
done
if [ -z "$missing" ]; then pass "/metrics"; else fail "/metrics lacks:$missing"; fi

if [ -n "${SSH_KEY:-}" ]; then
  log "ssh checks"
  ssh_vm() {
    ssh -p "$ssh_port" -i "$SSH_KEY" -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o LogLevel=ERROR root@127.0.0.1 "$@"
  }
  ssh_vm 'systemctl is-system-running; systemctl --failed --no-legend; findmnt -no SOURCE,FSTYPE / /var; pinna-bootctl slot-status; ls /run/systemd/dnssd' | sed 's/^/     /' || fail "ssh"
  if ssh_vm 'test -e /run/pinna/boot-protocol && test -e /run/pinna/ring && test -s /run/pinna/boot-ok && grep -qx "stage 6" /run/pinna/boot-stages'; then
    pass "boot stages handed over, verdict recorded (boot-protocol, ring, boot-ok, stage 6)"
  else
    fail "boot stage handover files in /run/pinna"
  fi
fi

# AirPlay (ADR 0011): off by default; on, pinnad starts the bridge and
# publishes shairport-sync's record; off stops both. Only the x86_64 image has a
# speaker (the virtual Echo Dot). The unit's state is read over ssh: the
# journal on the disk misses what happens this late.
if [ "$arch" = x86_64 ]; then
  log "AirPlay"
  airplay() {
    curl -fsS --max-time 10 -H 'Content-Type: application/json' -d "{\"airplay\":$1}" \
      "$web/pinna.v1.DeviceService/UpdateSettings"
  }
  if curl -fsS --max-time 10 -H 'Content-Type: application/json' -d '{}' \
       "$web/pinna.v1.DeviceService/GetStatus" | grep -q '"airplay":true'; then
    fail "AirPlay off by default"
  else
    pass "AirPlay off by default"
  fi
  if airplay true | grep -q '"airplay":true'; then pass "AirPlay switched on over the API"; else fail "AirPlay switched on over the API"; fi
  ready=
  for _ in $(seq 1 20); do
    if curl -fsS --max-time 10 -H 'Content-Type: application/json' -d '{}' \
         "$web/pinna.v1.DeviceService/GetStatus" | grep -q '"AIRPLAY_STATE_READY"'; then
      ready=1; break
    fi
    sleep 1
  done
  if [ -n "$ready" ]; then pass "AirPlay reported ready in Status"; else fail "AirPlay reported ready in Status"; fi
  if [ -n "${SSH_KEY:-}" ]; then
    sleep 5
    if ssh_vm 'systemctl is-active -q pinna-airplay && pgrep -x shairport-sync >/dev/null &&
               grep -q "^Type=_raop._tcp" /run/systemd/dnssd/pinna-airplay.dnssd &&
               [ "$(stat -c %G /run/pinna/stream.sock)" = pinna-stream ] &&
               ! systemctl is-active -q avahi-daemon'; then
      pass "AirPlay on: bridge and shairport-sync running, _raop._tcp published, socket shared"
    else
      fail "AirPlay on:"
      ssh_vm 'systemctl status pinna-airplay --no-pager -n 15; ls -l /run/pinna/stream.sock /run/systemd/dnssd' | sed 's/^/     /'
    fi
  fi
  if airplay false | grep -q '"airplay":true'; then fail "AirPlay switched off"; else pass "AirPlay switched off"; fi
  if [ -n "${SSH_KEY:-}" ]; then
    sleep 5 # the unit allows shairport-sync 3 s to stop
    if ssh_vm '! systemctl is-active -q pinna-airplay && ! test -e /run/systemd/dnssd/pinna-airplay.dnssd'; then
      pass "AirPlay off: bridge stopped, record withdrawn"
    else
      fail "AirPlay off: bridge stopped, record withdrawn"
      ssh_vm 'systemctl status pinna-airplay --no-pager -n 20; ls -l /run/systemd/dnssd; journalctl -b --no-pager -o short-monotonic | grep -E "airplay|shairport" | tail -20' | sed 's/^/     /'
    fi
  fi
fi

# Sendspin (ADR 0027): off by default; on, pinnad itself listens on TCP 8928
# (0x22E0 in /proc/net) and publishes _sendspin._tcp, and keeps its keys in a
# file only it reads; off withdraws the record and closes the port. The
# virtual Echo Dot has no hardware pointer, so it doesn't play in exact step.
if [ "$arch" = x86_64 ]; then
  log "Sendspin"
  sendspin() {
    curl -fsS --max-time 10 -H 'Content-Type: application/json' -d "{\"sendspin\":$1}" \
      "$web/pinna.v1.DeviceService/UpdateSettings"
  }
  listening='awk '\''$2 ~ /:22E0$/ && $4 == "0A"'\'' /proc/net/tcp6 /proc/net/tcp | grep -q .'
  if curl -fsS --max-time 10 -H 'Content-Type: application/json' -d '{}' \
       "$web/pinna.v1.DeviceService/GetStatus" | grep -q '"sendspin":true'; then
    fail "Sendspin off by default"
  else
    pass "Sendspin off by default"
  fi
  if sendspin true | grep -q '"sendspin":true'; then pass "Sendspin switched on over the API"; else fail "Sendspin switched on over the API"; fi
  reply=
  for _ in $(seq 1 20); do
    reply=$(curl -fsS --max-time 10 -H 'Content-Type: application/json' -d '{}' \
               "$web/pinna.v1.DeviceService/GetStatus" || true)
    if printf '%s' "$reply" | grep -q '"SENDSPIN_STATE_READY"'; then break; fi
    reply=
    sleep 1
  done
  if [ -n "$reply" ]; then pass "Sendspin reported ready in Status"; else fail "Sendspin reported ready in Status"; fi
  if printf '%s' "$reply" | grep -q '"exactTiming":true'; then
    fail "Sendspin not in exact step without a hardware pointer"
  else
    pass "Sendspin not in exact step without a hardware pointer"
  fi
  if [ -n "${SSH_KEY:-}" ]; then
    sleep 5
    if ssh_vm "$listening &&
               grep -q '^Type=_sendspin._tcp' /run/systemd/dnssd/pinna-sendspin.dnssd &&
               grep -q 'path=/sendspin' /run/systemd/dnssd/pinna-sendspin.dnssd &&
               [ \"\$(stat -c %a /var/lib/pinna/sendspin.toml)\" = 600 ]"; then
      pass "Sendspin on: port 8928 listening, _sendspin._tcp published, keys kept 0600"
    else
      fail "Sendspin on:"
      ssh_vm 'ls -l /run/systemd/dnssd /var/lib/pinna; cat /proc/net/tcp6; journalctl -b --no-pager -o short-monotonic | grep sendspin | tail -20' | sed 's/^/     /'
    fi
  fi
  if sendspin false | grep -q '"sendspin":true'; then fail "Sendspin switched off"; else pass "Sendspin switched off"; fi
  if [ -n "${SSH_KEY:-}" ]; then
    sleep 5 # the record goes with a restart of systemd-resolved
    if ssh_vm "! test -e /run/systemd/dnssd/pinna-sendspin.dnssd && ! { $listening; }"; then
      pass "Sendspin off: record withdrawn, port 8928 closed"
    else
      fail "Sendspin off: record withdrawn, port 8928 closed"
      ssh_vm 'ls -l /run/systemd/dnssd; cat /proc/net/tcp6; journalctl -b --no-pager -o short-monotonic | grep sendspin | tail -20' | sed 's/^/     /'
    fi
  fi
fi

log "stopping the VM and reading its journal from the userdata partition"
kill "$(cat "$work/qemu.pid")" 2>/dev/null || true
sleep 2
extract_journal "$work/disk.raw" "$work"
journalctl --directory="$work/journal/journal" --no-pager -o short-monotonic > "$work/journal.txt" 2>/dev/null || true
jcheck() { if grep -qE "$2" "$work/journal.txt"; then pass "$1"; else fail "$1"; fi; }
v_re=${version//./\\.}
jcheck "pinna-init: no bootloader, slot a" "pinna-init: no bootloader: slot a, as the preloader and LK would choose \(good"
jcheck "pinna-init booted slot a, version $version" "pinna-init: booting slot a, version ${v_re}$"
jcheck "root is the dm-verity device dm-0" "pinna-init: slot [a-z0-9]+ -> dm-0 \(dm-verity, root hash"
jcheck "kernel: erofs mounted from dm-0" "erofs: \(device dm-0\): mounted"
jcheck "/var formatted on first boot" "successfully formatted as ext4"
jcheck "/var mounted" "Mounted var.mount"
jcheck "multi-user.target reached" "Reached target multi-user.target"
# systemd taints itself when the kernel misses what it needs: the kernel
# config must meet its requirements. (unmerged-bin is Debian trixie's separate
# /usr/sbin, a userland matter.)
kernel_taints='old-kernel|cgroupsv1|overflow[ug]id-not-65534|short-[ug]id-range'
taint=$(grep -oE 'System is tainted: .*' "$work/journal.txt" | head -1)
if grep -qE "$kernel_taints" <<<"$taint"; then
  fail "systemd tainted by the kernel: $taint"
else
  pass "systemd not tainted by the kernel${taint:+ ($taint)}"
fi
jcheck "pinnad started" "Started pinnad.service"
jcheck "root's boot protocol recorded" "pinna-init: root speaks boot protocol 1"
jcheck "stage helper started" "pinna-bootstages: up"
jcheck "stage helper skips the VM's USB" "skipping the USB stage"
jcheck "stage helper skips the VM's Wi-Fi" "skipping the Wi-Fi stage"
jcheck "stage helper reaches stage 6" "pinna-bootstages: stage 6 \(Pinna running\) at"
jcheck "stage helper hands over to pinnad" "pinna-bootstages: pinnad claimed the ring at stage 6"
jcheck "pinnad takes the boot stages over" "took the boot stages over from the stage helper"
jcheck "pinnad reports a cold start" "last boot: no records: a cold start, or the power was lost"
jcheck "mDNS service published" "published _esphomelib._tcp"
jcheck "fastboot guard armed" "armed: next boot enters fastboot unless cleared"
jcheck "slot a marked good by the health check" "slot a marked good \(version ${v_re}\)"
jcheck "fastboot guard cleared by the health check" "cleared: boot marked healthy"
jcheck "health check finished" "Finished pinna-boot-ok.service"
read -r misc_first _ <<<"$(part_range "$work/disk.raw" misc)"
cmd=$(dd if="$work/disk.raw" bs=512 skip="$misc_first" count=1 status=none | head -c 32 | tr -d '\0')
if [ -z "$cmd" ]; then pass "misc bootloader command empty after boot"; else fail "misc bootloader command left set: $cmd"; fi
slot_status "$work/disk.raw" "$work" > "$work/slot-status.txt"
sed 's/^/     /' "$work/slot-status.txt"
if grep -qx "state: valid" "$work/slot-status.txt" && grep -qx "active: a" "$work/slot-status.txt" &&
   grep -q "^a: good" "$work/slot-status.txt"; then
  pass "misc slot state: active a, a good"
else
  fail "misc slot state"
fi
failed=$(grep -E "failed with result|Failed to start" "$work/journal.txt" || true)
if [ -z "$failed" ]; then pass "no failed units"; else fail "failed units:"; echo "$failed" | sed 's/^/     /'; fi
grep -m1 "Startup finished" "$work/journal.txt" | sed 's/^/     /' || true

if [ "$status" = 0 ]; then log "VM boot gate PASSED"; else log "VM boot gate FAILED (console: $work/console.log)"; fi
exit "$status"
