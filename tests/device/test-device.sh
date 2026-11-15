#!/usr/bin/env bash
# Hardware gate: the boot-gate checks plus what only the real Echo Dot can show,
# run against a device that already runs Pinna. Read-only except for playing a
# short clip through the speaker (acoustic loopback) and an LED test.
#
#   PINNA_HOST        ssh destination (default root@pinna.local)
#   PINNA_SSH         ssh command (default ssh)
#   PINNA_API_HOST    address for the ESPHome API from this machine
#                     (default: the device's wlan0 IPv4, read over ssh)
#   PINNA_API_KEY     file with the ESPHome Noise key (empty: plaintext)
#   MIN_THROUGHPUT    minimum Wi-Fi download rate in bytes/s (default 100000)
#   THROUGHPUT_URL    file to download for the throughput check (required for it)
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
host=${PINNA_HOST:-root@pinna.local}
read -r -a ssh <<<"${PINNA_SSH:-ssh}"
fails=0

ok()   { printf '\033[32mok\033[0m   %s\n' "$*"; }
bad()  { printf '\033[31mFAIL\033[0m %s\n' "$*"; fails=$((fails + 1)); }
info() { printf '     %s\n' "$*"; }
remote() { "${ssh[@]}" "$host" "$@"; }
check() { # check <description> <remote command>
  local desc=$1; shift
  if out=$(remote "$@" 2>&1); then ok "$desc"; else bad "$desc"; info "${out//$'\n'/ | }"; fi
}

printf '\033[1m==> %s\033[0m\n' "device $host"
remote true || { bad "ssh to $host"; exit 1; }

check "system running, no failed units" \
  'test "$(systemctl is-system-running)" = running || { systemctl --failed --no-legend; exit 1; }'
check "root is read-only erofs" 'findmnt -n -o FSTYPE,OPTIONS / | grep -q "^erofs .*\bro\b"'
check "/var is ext4 on userdata" 'findmnt -n -o SOURCE,FSTYPE /var | grep -q "ext4"'
check "Wi-Fi associated" 'wpa_cli -i wlan0 status | grep -qx wpa_state=COMPLETED'
check "wlan0 has IPv4 and a default route" 'ip -4 addr show wlan0 | grep -q inet && ip route | grep -q "^default .* wlan0"'
check "DNS resolves" 'resolvectl query deb.debian.org >/dev/null'
check "clock synchronized" 'timedatectl show -p NTPSynchronized --value | grep -qx yes'
check "mDNS record published" 'grep -q "^Type=_esphomelib._tcp" /run/systemd/dnssd/*.dnssd'
check "fastboot guard cleared, Kaeru present" 'pinna-bootctl status | grep -qx "command: Empty" && pinna-bootctl status | grep -qx "kaeru: true"'
check "pinnad healthy (control API)" 'pinnactl status >/dev/null'
check "microphone capture running" 'pgrep -x arecord >/dev/null && journalctl -u pinnad -b -o cat | grep -q "microphone running"'
check "LED ring driver present" 'ls /sys/bus/i2c/devices/*/frame >/dev/null'
check "LED test animation accepted" 'pinnactl led-test >/dev/null'
remote 'systemd-sysext status --no-pager | grep -q pinna-dev' && info "note: development sysext overlay active (just push)"

api_host=${PINNA_API_HOST:-$(remote "ip -4 -o addr show wlan0 | awk '{print \$4}' | cut -d/ -f1")}
key_args=()
[ -n "${PINNA_API_KEY:-}" ] && key_args=(--key-file "$PINNA_API_KEY")
if uv run -q --with aioesphomeapi==46.2.0 python -I "$root/tests/device/api_check.py" \
    --host "$api_host" "${key_args[@]}" --loopback /var/lib/pinna/test/alexa-what-time.wav \
    --stage "${ssh[*]} $host"; then
  ok "ESPHome API, entities and acoustic loopback (speaker -> mic -> wake word)"
else
  bad "ESPHome API / acoustic loopback"
fi

if [ -n "${THROUGHPUT_URL:-}" ]; then
  min=${MIN_THROUGHPUT:-100000}
  rate=$(remote "curl -sS -o /dev/null --max-time 15 -w '%{speed_download}' '$THROUGHPUT_URL'" 2>/dev/null | cut -d. -f1)
  if [ "${rate:-0}" -ge "$min" ]; then ok "Wi-Fi download ${rate} B/s (>= $min)"
  else bad "Wi-Fi download ${rate:-0} B/s (< $min)"; fi
else
  info "skipped Wi-Fi throughput (set THROUGHPUT_URL)"
fi

if [ "$fails" -eq 0 ]; then printf '\033[1;32m==> device gate PASSED\033[0m\n'
else printf '\033[1;31m==> device gate FAILED (%d)\033[0m\n' "$fails"; exit 1; fi
