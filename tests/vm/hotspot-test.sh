#!/usr/bin/env bash
# The setup hotspot (ADR 0028) on the virtual Echo Dot, with simulated radios.
#
# A satellite with no saved network runs "Pinna setup <name>". A phone joins
# it with the published password, gets an address with the satellite as
# router and DNS server, finds every name pointing at the satellite, is sent
# to the dashboard, and sees the hotspot in ListWifi. A join stops the
# hotspot; a wrong password brings it back; a network kept from its new
# address keeps it off; forgetting that network brings it back.
#
# Four radios (mac80211_hwsim, appended to the kernel's command line):
#   wlan0  the satellite's Wi-Fi
#   ap0    its hotspot (radio 1, renamed by mkosi/vm.extra)
#   wlan2  a phone, in the network namespace "phone"
#   wlan3  a home network, in the namespace "home": hostapd and busybox udhcpd
#
#   SSH_KEY=~/.ssh/key tests/vm/hotspot-test.sh
#
# Needs: just kernel-x86 image-x86 initramfs-x86, built with SSH_KEY's public
# key in secrets/authorized_keys; sgdisk on the host.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck source=lib.sh
source "$root/tests/vm/lib.sh"
[ -n "${SSH_KEY:-}" ] || { echo "SSH_KEY is required: the test drives the VM over ssh" >&2; exit 2; }
work=${WORK:-$root/build/vm-hotspot}
kernel=$root/build/kernel-x86/arch/x86/boot/bzImage
initrd=$root/build/initramfs-x86.cpio
image=$root/build/pinna-x86.root.img
ssh_port=${SSH_PORT:-2224}
timeout_s=${BOOT_TIMEOUT:-600}
home_ssid="Pinna test home"
home_pass="homepassword"

mkdir -p "$work"
busybox=$("$root/scripts/fetch-busybox.sh" amd64)
make_disk "$work/disk.raw" "$image"
rm -f "$work/console.log"

netdev="user,id=n0,hostfwd=tcp:127.0.0.1:${ssh_port}-:22"
log "booting $(basename "$image") with four simulated radios"
QEMU_APPEND="mac80211_hwsim.radios=4" \
  qemu_start x86_64 "$kernel" "$initrd" "$work/disk.raw" "$work/console.log" "$work/qemu.pid" "$netdev"
cleanup() {
  if [ -z "${KEEP:-}" ] && [ -f "$work/qemu.pid" ]; then
    kill "$(cat "$work/qemu.pid")" 2>/dev/null || true
  fi
}
trap cleanup EXIT

ssh_vm() {
  ssh -p "$ssh_port" -i "$SSH_KEY" -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
      -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o LogLevel=ERROR root@127.0.0.1 "$@"
}
# until SECONDS COMMAND...: run COMMAND on the VM every 2 s until it succeeds.
until_vm() {
  local secs=$1; shift
  for _ in $(seq 1 $((secs / 2))); do
    ssh_vm "$@" >/dev/null 2>&1 && return 0
    sleep 2
  done
  return 1
}
# A Connect JSON call to the dashboard as the satellite itself sees it, and as
# the phone on the hotspot sees it.
rpc() { ssh_vm "curl -sS --max-time 20 -H 'Content-Type: application/json' -d '$2' http://127.0.0.1/pinna.v1.DeviceService/$1"; }
phone_rpc() { ssh_vm "ip netns exec phone curl -sS --max-time 20 -H 'Content-Type: application/json' -d '$2' http://10.42.1.1/pinna.v1.DeviceService/$1"; }
# join PASSPHRASE [phone]: JoinWifi streams, so the request is one Connect
# envelope (flags 0, big-endian length) around the JSON.
join() {
  local body="{\"ssid\":\"$home_ssid\",\"security\":\"WIFI_SECURITY_WPA_PSK\",\"passphrase\":\"$1\"}"
  local n=${#body} curl="curl -sS --max-time 120" url=http://127.0.0.1
  [ "${2:-}" = phone ] && curl="ip netns exec phone curl -sS --max-time 90" url=http://10.42.1.1
  printf "\\x00\\x$(printf %02x $((n >> 24 & 255)))\\x$(printf %02x $((n >> 16 & 255)))\\x$(printf %02x $((n >> 8 & 255)))\\x$(printf %02x $((n & 255)))%s" "$body" |
    ssh_vm "cat > /tmp/join && $curl -H 'Content-Type: application/connect+json' \
      --data-binary @/tmp/join $url/pinna.v1.DeviceService/JoinWifi | tr -c '[:print:]' '\n'"
}
hotspot_on() { until_vm "$1" 'systemctl is-active -q pinna-hotspot && iw dev ap0 info | grep -q "type AP"'; }
hotspot_off() { until_vm "$1" '! systemctl is-active -q pinna-hotspot'; }

log "waiting for ssh"
until_vm "$timeout_s" true || { fail "no ssh within ${timeout_s}s"; console_text "$work/console.log" | tail -30; exit 1; }
pass "ssh"

log "the hotspot with no saved network"
if hotspot_on 180; then
  pass "pinna-hotspot runs hostapd on ap0"
else
  fail "no hotspot on ap0"
  ssh_vm 'ip -br link; systemctl status pinna-hotspot --no-pager -n 20; journalctl -b -u pinnad --no-pager | grep -i hotspot | tail' | sed 's/^/     /' || true
  exit 1
fi
name=$(ssh_vm hostname)
ssid=$(ssh_vm "iw dev ap0 info | sed -n 's/^[[:space:]]*ssid //p'")
if [ "$ssid" = "Pinna setup $name" ]; then pass "named \"$ssid\""; else fail "hotspot named \"$ssid\""; fi
if ssh_vm 'ip -4 -br addr show ap0' | grep -q '10.42.1.1/24'; then pass "ap0 has 10.42.1.1/24"; else fail "ap0's address"; fi

log "the phone and the home network"
ssh_vm 'cat > /tmp/busybox && chmod +x /tmp/busybox' <"$busybox"
# ssh joins its arguments into one command line: quote them for the VM's shell.
ssh_vm "bash -s -- $(printf '%q ' "$ssid" "$home_ssid" "$home_pass")" <<'EOF'
set -e
ssid=$1 home_ssid=$2 home_pass=$3
phy() { cat "/sys/class/net/$1/phy80211/name"; }
ip netns add phone
ip netns add home
iw phy "$(phy wlan2)" set netns name phone
iw phy "$(phy wlan3)" set netns name home

cat > /tmp/home.conf <<CONF
interface=wlan3
driver=nl80211
ssid=$home_ssid
hw_mode=g
channel=1
wpa=2
wpa_key_mgmt=WPA-PSK
rsn_pairwise=CCMP
wpa_passphrase=$home_pass
CONF
ip netns exec home ip link set lo up
ip netns exec home ip addr add 192.168.77.1/24 dev wlan3
ip netns exec home hostapd -B -P /tmp/home-hostapd.pid /tmp/home.conf >/dev/null
cat > /tmp/udhcpd.conf <<CONF
start 192.168.77.100
end 192.168.77.150
interface wlan3
lease_file /tmp/udhcpd.leases
opt router 192.168.77.1
CONF
touch /tmp/udhcpd.leases
ip netns exec home /tmp/busybox udhcpd /tmp/udhcpd.conf

cat > /tmp/phone.conf <<CONF
ctrl_interface=/run/phone-wpa
network={
  ssid="$ssid"
  psk="pinnasetup"
  key_mgmt=WPA-PSK
}
CONF
cat > /tmp/udhcpc.sh <<'SCRIPT'
#!/bin/sh
case "$1" in
  bound|renew)
    ip addr flush dev "$interface"
    ip addr add "$ip/$mask" dev "$interface"
    echo "ip=$ip router=$router dns=$dns" > /tmp/phone-lease ;;
esac
SCRIPT
chmod +x /tmp/udhcpc.sh
ip netns exec phone ip link set lo up
ip netns exec phone wpa_supplicant -B -i wlan2 -c /tmp/phone.conf -P /tmp/phone-wpa.pid >/dev/null
EOF
if until_vm 30 'ip netns exec phone wpa_cli -p /run/phone-wpa -i wlan2 status | grep -q wpa_state=COMPLETED'; then
  pass "the phone joins the hotspot with the password pinnasetup"
else
  fail "the phone didn't join the hotspot"
fi
lease=$(ssh_vm 'ip netns exec phone /tmp/busybox udhcpc -i wlan2 -n -q -t 10 -s /tmp/udhcpc.sh >/dev/null 2>&1; cat /tmp/phone-lease' || true)
if grep -q 'ip=10.42.1.' <<<"$lease" && grep -q 'router=10.42.1.1' <<<"$lease" && grep -q 'dns=10.42.1.1' <<<"$lease"; then
  pass "DHCP: $lease"
else
  fail "DHCP lease: ${lease:-none}"
fi

log "the captive portal, seen from the phone"
if ssh_vm 'ip netns exec phone /tmp/busybox nslookup connectivitycheck.gstatic.com 10.42.1.1' | grep -q 'Address: 10.42.1.1'; then
  pass "DNS answers every name with 10.42.1.1"
else
  fail "DNS on 10.42.1.1"
fi
probe=$(ssh_vm "ip netns exec phone curl -s -o /dev/null -w '%{http_code} %{redirect_url}' --max-time 10 \
  -H 'Host: connectivitycheck.gstatic.com' http://10.42.1.1/generate_204" || true)
if [ "$probe" = "302 http://10.42.1.1/#/network" ]; then pass "a connectivity check is sent to the dashboard"; else fail "connectivity check got: $probe"; fi
if ssh_vm 'ip netns exec phone curl -fsS --max-time 10 http://10.42.1.1/' | grep -q '<div id="app">'; then
  pass "the dashboard answers on the hotspot"
else
  fail "dashboard on 10.42.1.1"
fi
list=$(ssh_vm "ip netns exec phone curl -sS --max-time 10 -H 'Content-Type: application/json' -d '{}' \
  http://10.42.1.1/pinna.v1.DeviceService/ListWifi" || true)
if grep -q "\"hotspot\":\"$ssid\"" <<<"$list" && grep -q '"hasHotspot":true' <<<"$list"; then
  pass "ListWifi shows the hotspot"
else
  fail "ListWifi: $list"
fi
if phone_rpc ScanWifi '{}' | grep -q "\"ssid\":\"$home_ssid\""; then pass "the phone's scan finds the home network"; else fail "scan"; fi

log "a wrong password, from the phone"
join wrongpassword phone >"$work/join-wrong.txt" &
if hotspot_off 30; then pass "a join stops the hotspot"; else fail "the hotspot ran during the join"; fi
if hotspot_on 90; then pass "the hotspot comes back"; else fail "no hotspot after the failed join"; fi
wait || true
if grep -q 'setup hotspot off' "$work/join-wrong.txt"; then
  pass "the phone heard that the hotspot goes off"
else
  fail "no step reached the phone: $(tail -c 300 "$work/join-wrong.txt")"
fi
if until_vm 60 'ip netns exec phone wpa_cli -p /run/phone-wpa -i wlan2 status | grep -q wpa_state=COMPLETED' &&
   phone_rpc ListWifi '{}' | grep -q 'JOIN_FAILURE_WRONG_PASSWORD'; then
  pass "back on the hotspot, the phone reads why: wrong password"
else
  fail "the phone can't read the failed join"
fi

log "a join that is kept"
join "$home_pass" >"$work/join.txt" || true
address=$(grep -o '"address":"192.168.77.[0-9]*"' "$work/join.txt" | head -1 | cut -d'"' -f4 || true)
if grep -q 'JOIN_STAGE_TRIAL' "$work/join.txt" && [ -n "$address" ]; then
  pass "on trial at $address"
else
  fail "join: $(tail -c 300 "$work/join.txt")"
fi
if ! ssh_vm 'systemctl is-active -q pinna-hotspot'; then pass "no hotspot during the trial"; else fail "hotspot during the trial"; fi
ssh_vm "ip netns exec home curl -fsS --max-time 10 -o /dev/null http://$address/" || fail "dashboard at $address"
if until_vm 20 "curl -sS -H 'Content-Type: application/json' -d '{}' http://127.0.0.1/pinna.v1.DeviceService/ListWifi | grep -q '\"saved\":\\[{\"ssid\":\"$home_ssid\"'"; then
  pass "kept from the new address"
else
  fail "not kept: $(rpc ListWifi '{}')"
fi
sleep 40
if ! ssh_vm 'systemctl is-active -q pinna-hotspot'; then pass "the hotspot stays off with a saved network"; else fail "hotspot with a saved network"; fi

log "forgetting the last network"
rpc ForgetWifi "{\"ssid\":\"$home_ssid\"}" >/dev/null
if hotspot_on 90; then pass "the hotspot comes back"; else fail "no hotspot after forgetting"; fi

# The kernel asks for regulatory.db before the root is mounted;
# pinna-regdb.service loads it afterwards, so a country can apply.
if ssh_vm 'iw reg set DE && sleep 1 && iw reg get' | grep -q '^country DE:'; then
  pass "the regulatory database is loaded (country DE applies)"
else
  fail "the regulatory database isn't loaded: iw reg set DE had no effect"
fi

if [ "$status" = 0 ]; then log "setup hotspot test passed"; else log "setup hotspot test FAILED"; fi
exit "$status"
