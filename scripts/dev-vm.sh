#!/usr/bin/env bash
# The virtual Echo Dot (ADR 0014): boot the x86_64 image under KVM as a
# satellite you drive from its dashboard. The browser is its microphone,
# speaker and buttons; `just dev-ha` runs a Home Assistant for it.
#
#   just dev-vm                    # needs: just kernel-x86 image-x86
#   FRESH=1 just dev-vm            # start over with an empty /var
#   WEB_PORT=18190 SSH_PORT=2323 ESPHOME_PORT=6153 just dev-vm   # a second VM
#   ESPHOME_BIND=0.0.0.0 just dev-vm   # for a Home Assistant on your network
#
# The disk (build/dev-vm/disk.raw) persists: /var keeps Home Assistant's key
# and the settings. Each start writes the current image into slot a and
# resets the slot state, so the newest build always boots.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=../tests/vm/lib.sh
source "$root/tests/vm/lib.sh"

work=${WORK:-$root/build/dev-vm}
kernel=$root/build/kernel-x86/arch/x86/boot/bzImage
initrd=$root/build/initramfs-x86.cpio
image=$root/build/pinna-x86.root.img
web_port=${WEB_PORT:-18090}
ssh_port=${SSH_PORT:-2223}
esphome_port=${ESPHOME_PORT:-6053}
esphome_bind=${ESPHOME_BIND:-127.0.0.1}

for f in "$kernel:just kernel-x86" "$initrd:just initramfs-x86" "$image:just image-x86"; do
  [ -s "${f%%:*}" ] || { echo "missing ${f%%:*} (${f#*:})" >&2; exit 1; }
done
mkdir -p "$work"
if [ -f "$work/qemu.pid" ] && kill -0 "$(cat "$work/qemu.pid")" 2>/dev/null; then
  echo "a virtual Echo Dot is already running from $work (pid $(cat "$work/qemu.pid"))" >&2
  exit 1
fi

# zero_part DISK NAME: punch the partition back to zeros (a sparse write over an
# older image would leave its bytes wherever the new one has zeros).
zero_part() {
  local first last
  read -r first last <<<"$(part_range "$1" "$2")"
  fallocate --punch-hole --offset $((first * 512)) --length $(((last - first + 1) * 512)) "$1"
}

version=$(cargo run -q --manifest-path "$root/Cargo.toml" -p pinna-image -- info "$image" | sed -n 's/^version: //p')
if [ -n "${FRESH:-}" ] || [ ! -s "$work/disk.raw" ]; then
  log "new disk in $work"
  make_disk "$work/disk.raw" "$image"
else
  log "writing $version into slot a (keeping /var)"
  zero_part "$work/disk.raw" system_a
  part_write "$work/disk.raw" system_a "$image"
  zero_part "$work/disk.raw" misc
fi

rm -f "$work/console.log"
accel=tcg
[ -w /dev/kvm ] && accel=kvm
netdev="user,id=n0,hostfwd=tcp:127.0.0.1:${web_port}-:80,hostfwd=tcp:127.0.0.1:${ssh_port}-:22"
netdev+=",hostfwd=tcp:${esphome_bind}:${esphome_port}-:6053"
[ "$accel" = kvm ] || log "no access to /dev/kvm: the CPU is emulated, so this takes many minutes"
log "booting $version ($accel)"
qemu-system-x86_64 \
  -M q35,accel="$accel" -cpu max -smp 2 -m 1024 \
  -kernel "$kernel" -initrd "$initrd" \
  -drive if=none,file="$work/disk.raw",format=raw,id=hd0 \
  -device virtio-blk-pci,drive=hd0 \
  -netdev "$netdev" -device virtio-net-pci,netdev=n0 \
  -device virtio-rng-pci \
  -display none -serial file:"$work/console.log" \
  -pidfile "$work/qemu.pid" &
qemu=$!
stop() {
  kill "$qemu" 2>/dev/null || true
  wait "$qemu" 2>/dev/null || true
  rm -f "$work/qemu.pid"
}
trap stop EXIT
trap 'echo; log "stopping the virtual Echo Dot"; exit 0' INT TERM

web="http://127.0.0.1:${web_port}"
# Seconds to wait for the dashboard: without KVM, QEMU emulates the CPU.
wait_s=$([ "$accel" = kvm ] && echo 120 || echo 900)
for _ in $(seq 1 "$wait_s"); do
  kill -0 "$qemu" 2>/dev/null || {
    echo "QEMU exited. A port may be in use: set WEB_PORT, SSH_PORT or ESPHOME_PORT." >&2
    exit 1
  }
  curl -fsS --max-time 2 -o /dev/null "$web/" 2>/dev/null && break
  sleep 1
done
curl -fsS --max-time 2 -o /dev/null "$web/" ||
  { echo "the dashboard did not come up; console: $work/console.log" >&2; exit 1; }

if [ "$esphome_bind" = 0.0.0.0 ]; then
  lan=$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for (i = 1; i < NF; i++) if ($i == "src") print $(i + 1)}')
  ha="add ESPHome at ${lan:-your LAN address}, port $esphome_port (open it in your firewall)"
else
  ha="just dev-ha   (Home Assistant on this computer, set up for this satellite)"
fi
cat <<EOF

  Virtual Echo Dot $version is up.

  Dashboard        $web
                   open it in a browser on this computer (the microphone needs localhost)
  Home Assistant   $ha
  Dashboard dev    cd dashboard && PINNA_URL=$web npm run dev
  New pinnad       just push-vm   (SSH_PORT=$ssh_port)
  SSH              ssh -p $ssh_port root@127.0.0.1
  Console          $work/console.log

  Ctrl-C stops it.
EOF
wait "$qemu"
