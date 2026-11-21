#!/usr/bin/env bash
# Fetch Amazon's firmware into blobs/firmware/ and verify its sha256. The
# files are Amazon's, unmodified, and carry no license, so they are never
# committed (blobs/ is gitignored, ADR 0001). They are kept at
# https://static.iodev.org/pinna/firmware/<board>/<Fire OS release>/, each from
# the Fire OS update Amazon publishes for that board; the release is the one
# the files were checked against:
#
#   biscuit/fireos-6.5.7.5-8344/: Echo Dot (2nd gen), Fire OS 6.5.7.5
#     (NS6575/8344), update-kindle-biscuit_puffin-NS6575_user_8344_0013323245700.bin
#     (md5 155660387888642f34544348eb48d9a9). Into blobs/firmware/:
#     - WIFI_RAM_CODE_8163, ROMv2_lm_patch_1_{0,1}_hdr.bin, WMT_SOC.cfg: the
#       MT8163 Wi-Fi and Bluetooth firmware, /vendor/firmware of its system
#       image. The image installs them as /usr/lib/firmware/.
#     - i2s_to_spi_v34.bin: the microphone FPGA bitstream, built into its stock
#       kernel, and into Pinna's (CONFIG_EXTRA_FIRMWARE, ADR 0006).
#   donut/fireos-6574.1-8138/: Echo Dot (3rd gen), Fire OS 6574.1
#     (NS65741/8138), update-kindle-donut_puffin-NS65741_user_8138_0013222529668.bin
#     (sha256 ac22b78cf94c2ebacfa90447b770b803d9f72f35858d5218975d97fea95a0245).
#     Into blobs/firmware/donut/: the MT7668 Wi-Fi firmware from /vendor/firmware
#     of its system image (ADR 0031). Amazon's mt7668_patch_e2_hdr.bin is kept
#     there for reference only and not fetched: under that name the driver
#     would load it, and the MT8516 Echos use linux-firmware's
#     mediatek/mt7668pr2h.bin instead.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${1:-$root/blobs/firmware}"
base=https://static.iodev.org/pinna/firmware
biscuit=biscuit/fireos-6.5.7.5-8344
donut=donut/fireos-6574.1-8138

# <path under $base> <directory under $dest> <sha256>
files=(
  "$biscuit/WIFI_RAM_CODE_8163 . 9669cc9b03cfdc5e8fd4fd6e14c4c4050e8c196738ca4707eea12f14a6a8e64c"
  "$biscuit/ROMv2_lm_patch_1_0_hdr.bin . 6642cfd4fd67accf27b0f9615e4cd0941417ddbc3b5bef1004fe1eb59235146f"
  "$biscuit/ROMv2_lm_patch_1_1_hdr.bin . c4aa6dfc54b82f496d97be8eb3979232730f51a3fdfad34192b91d0b87b06a56"
  "$biscuit/WMT_SOC.cfg . 302bd4462de99c028c04092e561c1500d65582ce42a93c4c72ccae6e2c99013d"
  "$biscuit/i2s_to_spi_v34.bin . 77a558bacdaaf9e343f02f2d74f27a5f2bb2dc8b6d66cc2499b60ed14ef62fe6"
  "$donut/WIFI_RAM_CODE_MT7668.bin donut 9240c5182c89bdd98bda2655e01053ef8ea06afd45d61cf6ada3f11d65956e7d"
  "$donut/WIFI_RAM_CODE2_SDIO_MT7668.bin donut 35faff6a613fefcbe33a6f6d8df885be76c546f9e78ba8ab93056a816ad87254"
  "$donut/TxPwrLimit_MT76x8.dat donut fe9a33d42f702356ff9b458c4f9ad9d96be8a63869bacd9c59fd00176dcbe229"
  "$donut/wifi.cfg donut 1a97dfd343fc7b7635cab4b45bd638003320c371d9331d18a4d1d3e13d55a5b0"
  "$donut/EEPROM_MT7668.bin donut 836cf1851067d00f96fc5dbe79bbfb979b86aa1b985dc770a83b4ab6e570835a"
)

command -v curl >/dev/null || { echo "curl not found" >&2; exit 1; }
for entry in "${files[@]}"; do
  read -r path dir sha <<<"$entry"
  name="${path##*/}"
  file="$dest/$dir/$name"
  shown="${file#"$dest"/}"
  shown="${shown#./}"
  if [[ -f $file ]] && [[ "$(sha256sum "$file" | cut -d' ' -f1)" == "$sha" ]]; then
    echo "ok       $shown"
    continue
  fi
  echo "fetching $shown"
  mkdir -p "$(dirname "$file")"
  curl -fsSL --retry 3 -o "$file.part" "$base/$path"
  got="$(sha256sum "$file.part" | cut -d' ' -f1)"
  if [[ $got != "$sha" ]]; then
    rm -f "$file.part"
    echo "sha256 mismatch for $path: expected $sha, got $got" >&2
    exit 1
  fi
  mv "$file.part" "$file"
done
echo "firmware ready in $dest"
