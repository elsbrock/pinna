#!/usr/bin/env bash
# A/B update and automatic rollback under QEMU (armhf, TCG), in one VM run:
#
#   N    = build/pinna.root.img (just image), flashed to system_a
#   N+1  = a fresh build, served from the host over HTTP
#   N+2  = a fresh build whose pinnad never starts (tests/vm/broken-pinnad),
#          so its boot health check fails
#
#   boot 1  slot a, N     marked good; installs N+1 into b, reboots
#   boot 2  slot b, N+1   trial (guard not armed), marked good; installs N+2
#                         into a with 1 attempt (--tries 1), reboots
#   boot 3  slot a, N+2   trial, health check fails -> reboot
#   boot 4  slot b, N+1   a has no attempts left: rolled back
#
# The VM kernel says pinna.bootloader=none, so pinna-init counts the boots on
# the bootloader's slot record as the Echo's preloader does (ADR 0019).
#
# Commands run inside the VM come from a test-only systemd-sysext written into
# the VM's /var (userdata) before the first boot; it is never part of an image
# and needs no SSH or other remote access. Results are read from the VM's
# journal (all four boots) and its misc partition after power-off.
#
#   tests/vm/update-test.sh
#   REUSE_IMAGES=1 tests/vm/update-test.sh   # keep N+1/N+2 from a previous run
#   KEEP=1 tests/vm/update-test.sh           # leave the VM running
#
# Needs: just kernel-qemu image initramfs; the signing key (just keygen).
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck source=lib.sh
source "$root/tests/vm/lib.sh"
work=${WORK:-$root/build/vm-update}
kernel=$root/build/kernel-qemu/arch/arm/boot/zImage
initrd=$root/build/initramfs.cpio
base=$root/build/pinna.root.img
port=${HTTP_PORT:-18080}
timeout_s=${UPDATE_TIMEOUT:-1800}

image_tool() { cargo run -q --release --manifest-path "$root/Cargo.toml" -p pinna-image -- "$@"; }

mkdir -p "$work/www/n1" "$work/www/n2"
v0=$(image_tool info "$base" | sed -n 's/^version: //p')

# --- images ------------------------------------------------------------------
build_variant() { # dir version [extra tree]
  local dir=$1 v=$2 extra=()
  [ -n "${3:-}" ] && extra=(--extra-tree "$3")
  rm -f "${work:?}/www/$dir"/*.root.img
  "$root/scripts/build-root.sh" --version "$v" --output-dir "$work/mkosi-$dir" \
    "${extra[@]}" --out "$work/www/$dir/pinna-$v.root.img" > "$work/build-$dir.log" 2>&1 ||
    { tail -30 "$work/build-$dir.log"; return 1; }
  rm -rf "${work:?}/mkosi-$dir"
  tail -1 "$work/build-$dir.log" | sed 's/^/     /'
}
if [ -n "${REUSE_IMAGES:-}" ] && compgen -G "$work/www/n1/*.root.img" >/dev/null &&
   compgen -G "$work/www/n2/*.root.img" >/dev/null; then
  log "reusing N+1 and N+2 from a previous run"
else
  # Semantic versions (ADR 0022); the updater only compares them for channels.
  stamp="0.0.1-test.$(date -u +%Y%m%d%H%M).g$(git -C "$root" rev-parse --short=7 HEAD)"
  log "building N+1 (mkosi, a few minutes)"
  build_variant n1 "$stamp.n1"
  log "building the broken N+2 (pinnad never starts)"
  build_variant n2 "$stamp.n2-broken" "$root/tests/vm/broken-pinnad"
fi
img1=$(ls "$work"/www/n1/*.root.img)
img2=$(ls "$work"/www/n2/*.root.img)
v1=$(image_tool info "$img1" | sed -n 's/^version: //p')
v2=$(image_tool info "$img2" | sed -n 's/^version: //p')
image_tool manifest --key "$root/secrets/release.key" --out "$work/www/n1" "$img1" >/dev/null
image_tool manifest --key "$root/secrets/release.key" --out "$work/www/n2" "$img2" >/dev/null
: > "$work/www/done"
: > "$work/www/failed"
log "N = $v0, N+1 = $v1, N+2 (broken) = $v2"

# --- test-only sysext in the VM's /var ------------------------------------------
seed=$work/seed
rm -rf "${work:?}/seed"
ext=$seed/lib/extensions/pinna-test
mkdir -p "$ext/usr/lib/extension-release.d" "$ext/usr/lib/systemd/system/multi-user.target.d" \
         "$ext/usr/libexec/pinna-test" "$seed/lib/pinna-test"
# The manager reloads after the merge only if the extension asks for it.
printf 'ID=_any\nEXTENSION_RELOAD_MANAGER=1\n' > "$ext/usr/lib/extension-release.d/extension-release.pinna-test"
cat > "$ext/usr/lib/systemd/system/pinna-test-update.service" <<'EOF'
[Unit]
Description=Pinna VM test: one step of the A/B update test (test-only sysext)
Wants=network-online.target
After=pinna-boot-ok.service network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/libexec/pinna-test/step
EOF
# Units merged by systemd-sysext arrive after the boot transaction was built,
# so a .wants link is not acted on; Upholds= on the target is.
printf '[Unit]\nUpholds=pinna-test-update.service\n' \
  > "$ext/usr/lib/systemd/system/multi-user.target.d/pinna-test.conf"
cat > "$ext/usr/libexec/pinna-test/step" <<EOF
#!/bin/sh
# Test only: one step of tests/vm/update-test.sh per boot. Always exits 0
# (Upholds= would restart it); failures are reported to the host instead.
set -u
host=http://10.0.2.2:$port
report() { journalctl --sync; sync; curl -fsS -o /dev/null "\$host/\$1" || true; }
update() { # dir tries
  pinnactl update "\$host/\$1/manifest.json" --tries "\$2" --reboot && exit 0
  echo "pinna-test: update to \$1 failed"
  report "failed?step=\$1"
  exit 0
}
v=\$(cat /run/pinna/boot-version)
s=\$(cat /run/pinna/boot-slot)
echo "pinna-test: booted slot \$s, version \$v"
pinna-bootctl slot-status
case "\$v" in
  "$v0")
    update n1 3 ;;
  "$v1")
    if [ -e /var/lib/pinna-test/n2-tried ]; then
      echo "pinna-test: back on \$v in slot \$s after the broken update"
      report "done?slot=\$s&version=\$v"
      exit 0
    fi
    touch /var/lib/pinna-test/n2-tried
    sync
    update n2 1 ;;
  *)
    echo "pinna-test: nothing to do on \$v" ;;
esac
EOF
chmod 0755 "$ext/usr/libexec/pinna-test/step"

# userdata: ext4 with the seed, everything owned by root.
rm -f "$work/userdata.seed.ext4"
mkfs.ext4 -q -F -L userdata -E root_owner=0:0 -d "$seed" "$work/userdata.seed.ext4" 512M
(cd "$seed" && find . -mindepth 1 | sed 's|^\.||') | while read -r p; do
  printf 'sif "%s" uid 0\nsif "%s" gid 0\n' "$p" "$p"
done | debugfs -w -f - "$work/userdata.seed.ext4" >/dev/null 2>&1

make_disk "$work/disk.raw" "$base"
part_write "$work/disk.raw" userdata "$work/userdata.seed.ext4"

# --- run -------------------------------------------------------------------------
rm -f "$work/console.log" "$work/http.log"
python3 -m http.server --bind 127.0.0.1 --directory "$work/www" "$port" 2> "$work/http.log" >/dev/null &
http_pid=$!
cleanup() {
  kill "$http_pid" 2>/dev/null || true
  if [ -z "${KEEP:-}" ] && [ -f "$work/qemu.pid" ]; then
    kill "$(cat "$work/qemu.pid")" 2>/dev/null || true
  fi
}
trap cleanup EXIT

log "booting N from slot a under QEMU; the VM updates itself from http://10.0.2.2:$port"
qemu_start armhf "$kernel" "$initrd" "$work/disk.raw" "$work/console.log" "$work/qemu.pid" "user,id=n0"
seen=0
start=$SECONDS
done=
while [ $((SECONDS - start)) -lt "$timeout_s" ]; do
  sleep 10
  mapfile -t lines < <(console_text "$work/console.log" | grep -aE "pinna-init: (booting|no bootloader: slot|slot .* not bootable|slot .* retired)" || true)
  for l in "${lines[@]:$seen}"; do printf '     [%4ss] %s\n' $((SECONDS - start)) "${l#*] }"; done
  seen=${#lines[@]}
  if grep -q '"GET /done' "$work/http.log"; then done=1; break; fi
  if grep -q '"GET /failed' "$work/http.log"; then fail "the VM reported a failed update"; break; fi
  kill -0 "$(cat "$work/qemu.pid")" 2>/dev/null || { fail "QEMU exited"; break; }
done
[ -n "$done" ] || fail "the VM did not report back within ${timeout_s}s"
grep -aE '"GET /' "$work/http.log" | sed 's/^.*"GET /     GET /; s/ HTTP.*//' || true

log "stopping the VM and reading its journal (all boots) and misc"
kill "$(cat "$work/qemu.pid")" 2>/dev/null || true
sleep 2
extract_journal "$work/disk.raw" "$work"
jdir=$work/journal/journal
boots=$(journalctl --directory="$jdir" --list-boots --no-pager 2>/dev/null | grep -c . || true)
boots=$((boots > 0 ? boots - 1 : 0))  # header line
for i in $(seq 1 "$boots"); do
  journalctl --directory="$jdir" -b "$i" --no-pager -o short-monotonic > "$work/boot$i.txt" 2>/dev/null || true
done
if [ "$boots" = 4 ]; then pass "4 boots"; else fail "expected 4 boots, journal has $boots"; fi

bcheck() { # boot description regex
  if [ -f "$work/boot$1.txt" ] && grep -qE "$3" "$work/boot$1.txt"; then pass "boot $1: $2"; else fail "boot $1: $2"; fi
}
re() { printf '%s' "${1//./\\.}"; }
bcheck 1 "slot a, N ($v0)" "pinna-init: booting slot a, version $(re "$v0")$"
bcheck 1 "root on dm-verity" "pinna-init: slot [a-z0-9]+ -> dm-0 \(dm-verity"
bcheck 1 "slot a marked good" "slot a marked good \(version $(re "$v0")\)"
bcheck 1 "N+1 downloaded, verified and installed into slot b" "installed version $(re "$v1") into slot b"
bcheck 2 "trial boot of slot b" "pinna-init: no bootloader: slot b, as the preloader and LK would choose \(trial attempts=2"
bcheck 2 "slot b, N+1 ($v1)" "pinna-init: booting slot b, version $(re "$v1")$"
bcheck 2 "fastboot guard not armed during the trial" "not arming: trial boot of slot b"
bcheck 2 "slot b marked good" "slot b marked good \(version $(re "$v1")\)"
bcheck 2 "N+2 installed into slot a with 1 attempt" "installed version $(re "$v2") into slot a: the next boot tries it \(1 attempts\)"
bcheck 3 "trial boot of slot a" "pinna-init: no bootloader: slot a, as the preloader and LK would choose \(trial attempts=0"
bcheck 3 "slot a, N+2 ($v2)" "pinna-init: booting slot a, version $(re "$v2")$"
bcheck 3 "fastboot guard not armed during the trial" "not arming: trial boot of slot a"
bcheck 3 "pinnad does not start" "pinnad.service.*(skipped|unmet condition)"
bcheck 3 "health check fails" "pinna-boot-ok.service: Failed with result"
# systemd logs nothing for FailureAction= and journald lets go of /var before
# reboot.target: the console shows the reboot, and nothing else asked for it.
seg3=$(console_text "$work/console.log" | awk '/pinna-init: booting/ {n++} n == 3')
if grep -q "Reached target reboot.target" <<<"$seg3" && ! grep -q "rebooting into slot" "$work/boot3.txt"; then
  pass "boot 3: the failed check reboots (FailureAction=reboot)"
else
  fail "boot 3: the failed check reboots (FailureAction=reboot)"
fi
if grep -q "slot a marked good" "$work/boot3.txt" 2>/dev/null; then fail "boot 3: broken slot a was marked good"; else pass "boot 3: broken slot a not marked good"; fi
bcheck 4 "rolled back: slot a has no attempts left" "pinna-init: no bootloader: slot b, as the preloader and LK would choose \(good"
bcheck 4 "slot b, N+1 ($v1) again" "pinna-init: booting slot b, version $(re "$v1")$"
bcheck 4 "fastboot guard armed on the good slot" "armed: next boot enters fastboot unless cleared"
bcheck 4 "health check passes" "slot b marked good \(version $(re "$v1")\)"
bcheck 4 "the test saw the rollback" "pinna-test: back on $(re "$v1") in slot b"

slot_status "$work/disk.raw" "$work" > "$work/slot-status.txt"
sed 's/^/     /' "$work/slot-status.txt"
if grep -qx "active: b" "$work/slot-status.txt" && grep -q "^b: good" "$work/slot-status.txt" &&
   grep -q "^a: unbootable attempts=0" "$work/slot-status.txt"; then
  pass "misc slot state: active b (good), a unbootable"
else
  fail "misc slot state"
fi
echo "     total $((SECONDS - start))s in the VM"

if [ "$status" = 0 ]; then log "VM update test PASSED"; else log "VM update test FAILED (console: $work/console.log, journals: $work/boot*.txt)"; fi
exit "$status"
