#!/usr/bin/env bash
# Gather everything that goes into the image but is either built here or
# owner-local (never committed) into build/rootfs-extra, which mkosi copies on
# top of the Debian root.
#
#   blobs/firmware/          MT8163 WMT/Wi-Fi firmware (from the device's stock system partition)
#   blobs/models/            openWakeWord models (CC BY-NC-SA, fetched by `just models`)
#   build/wmt/               static WMT helpers (`just wmt`)
#   target/<arm>/release/    pinnad, pinna-bootctl (`just rust-arm`)
#   secrets/                 authorized_keys, wifi/wpa_supplicant-wlan0.conf
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
out=$root/build/rootfs-extra
arm=$root/target/armv7-unknown-linux-musleabihf/release

rm -rf "${out:?}"
mkdir -p "$out"

warn() { printf 'assemble: WARNING: %s\n' "$*" >&2; }

# Firmware
if compgen -G "$root/blobs/firmware/*" >/dev/null; then
  install -d "$out/usr/lib/firmware"
  install -m0644 "$root"/blobs/firmware/* "$out/usr/lib/firmware/"
else
  warn "no firmware in blobs/firmware — Wi-Fi will not come up on the device"
fi

# WMT helpers
if compgen -G "$root/build/wmt/wmt_*" >/dev/null; then
  install -d "$out/usr/libexec/pinna"
  install -m0755 "$root"/build/wmt/wmt_* "$out/usr/libexec/pinna/"
else
  warn "WMT helpers not built (just wmt)"
fi

# Rust binaries
install -d "$out/usr/bin"
for bin in pinnad pinna-bootctl; do
  if [ -x "$arm/$bin" ]; then
    install -m0755 "$arm/$bin" "$out/usr/bin/"
  else
    warn "$bin not built for armv7 (just rust-arm)"
  fi
done

# Wake word models
if [ -d "$root/blobs/models" ]; then
  install -d "$out/usr/share/pinna"
  cp -r "$root/blobs/models" "$out/usr/share/pinna/models"
else
  warn "no wake word models (just models)"
fi

# Owner-local credentials
if [ -s "$root/secrets/authorized_keys" ]; then
  install -d -m0700 "$out/root/.ssh"
  install -m0600 "$root/secrets/authorized_keys" "$out/root/.ssh/authorized_keys"
else
  warn "no secrets/authorized_keys — SSH login will be impossible"
fi
if [ -s "$root/secrets/wifi/wpa_supplicant-wlan0.conf" ]; then
  install -d "$out/etc/wpa_supplicant"
  install -m0600 "$root/secrets/wifi/wpa_supplicant-wlan0.conf" "$out/etc/wpa_supplicant/"
else
  warn "no Wi-Fi credentials — the device is reachable over USB only"
fi

find "$out" -type f | sed "s|$out||" | sort
