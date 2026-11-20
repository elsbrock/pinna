#!/usr/bin/env bash
# Gather everything that goes into the image but is either built here, vendor
# firmware or owner-local (never committed) into build/rootfs-extra, which
# mkosi copies on top of the Debian root.
#
#   blobs/firmware/          MT8163 Wi-Fi/BT firmware (Amazon's, fetched by `just firmware`)
#   blobs/models/            openWakeWord models (CC BY-NC-SA, fetched by `just models`)
#   build/wmt/               static WMT helpers (`just wmt`)
#   target/<arm>/release/    pinnad, pinnactl, pinna-bootctl (`just rust-arm`)
#   target/<gnu>/release/    libpinna_ladspa.so, glibc (`just ladspa`)
#   dashboard/dist/          the dashboard's static files (`just web`)
#   secrets/                 authorized_keys, wifi/wpa_supplicant-wlan0.conf
#   mkosi/vm.extra/          the virtual Echo Dot (x86_64 only, ADR 0014)
#
# PINNA_RELEASE=1 refuses any owner file in secrets/ above and needs the wake
# word models (ADR 0015).
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

# A release image carries nothing of a particular owner (ADR 0015).
if [ "${PINNA_RELEASE:-}" = 1 ]; then
  for f in authorized_keys wifi/wpa_supplicant-wlan0.conf device/pinnad.toml device/esphome-key; do
    if [ -e "$root/secrets/$f" ]; then
      echo "assemble: PINNA_RELEASE=1, but secrets/$f exists; a release image must not carry it" >&2
      exit 1
    fi
  done
  [ -d "$root/blobs/models" ] || { echo "assemble: PINNA_RELEASE=1 needs the wake word models (just models)" >&2; exit 1; }
fi

# Firmware (device only)
if [ "$device" = yes ]; then
  fw=$root/blobs/firmware
  [ -f "$fw/WIFI_RAM_CODE_8163" ] || { echo "assemble: $fw is missing the Wi-Fi/BT firmware (just firmware)" >&2; exit 1; }
  install -d "$out/usr/lib/firmware"
  install -m0644 "$fw"/WIFI_RAM_CODE_8163 "$fw"/ROMv2_lm_patch_1_*_hdr.bin \
    "$fw"/WMT_SOC.cfg "$out/usr/lib/firmware/"
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
for bin in pinnad pinnactl pinna-bootctl pinna-bootstages pinna-airplay pinna-bt; do
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

# Dashboard (served by pinnad's [web] listener)
if [ -s "$root/dashboard/dist/index.html" ]; then
  install -d "$out/usr/share/pinna"
  cp -r "$root/dashboard/dist" "$out/usr/share/pinna/web"
else
  warn "dashboard not built (just web) — pinnad serves the API only on port 80"
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
# The virtual Echo Dot (x86_64): its PipeWire graph, pinnad's [dev] layer and
# the unit drop-ins that turn them on.
if [ "$device" = no ]; then
  cp -r "$root/mkosi/vm.extra/." "$out/"
fi

# Owner-local device identity and Home Assistant API key: defaults only, seeded
# to /var/lib/pinna on first boot (tmpfiles) so a re-flash keeps the same
# Home Assistant device. Not in the VM: it would join Home Assistant as the
# owner's Echo Dot.
for f in pinnad.toml esphome-key; do
  [ "$device" = yes ] || break
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
