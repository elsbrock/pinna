#!/usr/bin/env bash
# Gather everything that goes into the image but is either built here or
# owner-local (never committed) into build/rootfs-extra, which mkosi copies on
# top of the Debian root.
#
#   blobs/firmware/          MT8163 WMT/Wi-Fi firmware (from the device's stock system partition)
#   blobs/models/            openWakeWord models (CC BY-NC-SA, fetched by `just models`)
#   build/wmt/               static WMT helpers (`just wmt`)
#   target/<arm>/release/    pinnad, pinnactl, pinna-bootctl (`just rust-arm`)
#   target/<gnu>/release/    libpinna_ladspa.so, glibc (`just ladspa`)
#   secrets/                 authorized_keys, wifi/wpa_supplicant-wlan0.conf
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# --arch x86_64 assembles build/rootfs-extra-x86 for the VM smoke-test image.
case "${1:-}" in
  --arch) target_arch=${2:?} ;;
  *) target_arch=armv7 ;;
esac
if [ "$target_arch" = x86_64 ]; then
  out=$root/build/rootfs-extra-x86
  arm=$root/target/x86_64-unknown-linux-musl/release
  ladspa=$root/target/x86_64-unknown-linux-gnu/release/libpinna_ladspa.so
  device=no
else
  out=$root/build/rootfs-extra
  arm=$root/target/armv7-unknown-linux-musleabihf/release
  ladspa=$root/target/armv7-unknown-linux-gnueabihf/release/libpinna_ladspa.so
  device=yes
fi

rm -rf "${out:?}"
mkdir -p "$out"

warn() { printf 'assemble: WARNING: %s\n' "$*" >&2; }

# Firmware (device only)
if [ "$device" = no ]; then
  :
elif compgen -G "$root/blobs/firmware/*" >/dev/null; then
  install -d "$out/usr/lib/firmware"
  install -m0644 "$root"/blobs/firmware/* "$out/usr/lib/firmware/"
else
  warn "no firmware in blobs/firmware — Wi-Fi will not come up on the device"
fi

# WMT helpers (device only)
if [ "$device" = no ]; then
  :
elif compgen -G "$root/build/wmt/wmt_*" >/dev/null; then
  install -d "$out/usr/libexec/pinna"
  install -m0755 "$root"/build/wmt/wmt_* "$out/usr/libexec/pinna/"
else
  warn "WMT helpers not built (just wmt)"
fi

# Rust binaries
install -d "$out/usr/bin"
for bin in pinnad pinnactl pinna-bootctl; do
  if [ -x "$arm/$bin" ]; then
    install -m0755 "$arm/$bin" "$out/usr/bin/"
  else
    warn "$bin not built for $target_arch (just rust-arm / rust-x86)"
  fi
done

# PipeWire LADSPA plugin (glibc, loaded by Debian's PipeWire)
if [ -f "$ladspa" ]; then
  install -Dm0644 "$ladspa" "$out/usr/lib/ladspa/pinna-beam.so"
else
  warn "pinna-beam.so not built for $target_arch (just ladspa)"
fi

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
# Owner-local device identity and Home Assistant API key: defaults only, seeded
# to /var/lib/pinna on first boot (tmpfiles) so a re-flash keeps the same
# Home Assistant device.
for f in pinnad.toml esphome-key; do
  if [ -s "$root/secrets/device/$f" ]; then
    install -d "$out/usr/share/pinna/device"
    install -m0600 "$root/secrets/device/$f" "$out/usr/share/pinna/device/$f"
  fi
done
if [ -s "$root/secrets/wifi/wpa_supplicant-wlan0.conf" ]; then
  # Default only: first boot copies it to /var/lib/pinna/wifi/ (tmpfiles), which
  # /etc/wpa_supplicant/wpa_supplicant-wlan0.conf points at, so Wi-Fi can be
  # changed on the device without a new image.
  install -d "$out/usr/share/pinna/wifi"
  install -m0600 "$root/secrets/wifi/wpa_supplicant-wlan0.conf" "$out/usr/share/pinna/wifi/"
else
  warn "no Wi-Fi credentials — the device is reachable over USB only"
fi

find "$out" -type f | sed "s|$out||" | sort
