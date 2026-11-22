#!/usr/bin/env bash
# Build the Pinna kernel for the Echo (2nd gen) family and package it as a
# Debian package.
#
#   kernel/build.sh            -> build/kernel/{zImage,vmlinux,.config} + build/debs/linux-image-*.deb
#   kernel/build.sh qemu       -> build/kernel-qemu/ (ARM QEMU virt variant for boot tests, no package)
#   kernel/build.sh qemu-arm64 -> build/kernel-qemu-arm64/ (arm64 QEMU virt variant that boots the
#                                 armhf image as 32-bit EL0, ADR 0031; no package)
#   kernel/build.sh mt8516     -> build/kernel-mt8516/{Image.gz,dtbs} (the MT8516 Echos, ADR 0031;
#                                 no package yet)
#   kernel/build.sh mt8519     -> build/kernel-mt8519/{zImage,cannoli-hvt.dtb,cannoli-dvt-plastic.dtb}
#                                 (the Echo Dot (5th gen), 32-bit ARM, ADR 0034; no package yet)
#   kernel/build.sh x86        -> build/kernel-x86/ (x86_64 QEMU/KVM variant for smoke tests, no package)
#
#   DEBUG=1       also merge kernel/debug.config (ftrace, debugfs, sysrq, …)
#   CHECK_ONLY=1  stop after the config assertions
#
# Source: Debian's linux package (upstream stable + Debian's patch series, see
# source.env), then kernel/patches/*.patch. Config: allnoconfig + common.config
# + the variant's fragment (+ debug.config).
#
# Requires the dev shell (cross gcc 15, make, dtc, dpkg).
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")
# shellcheck source=source.env
source "$here/source.env"

variant=${1:-device}
src=$root/build/linux
deb=$root/build/linux-debian
arch=arm
cross=armv7l-unknown-linux-gnueabihf-
# The armhf cross compiler, for the vDSO an arm64 kernel gives 32-bit processes.
compat=
fragments=("$here/common.config")
case "$variant" in
  device) out=$root/build/kernel; fragments+=("$here/device.config"); targets=(zImage mediatek/mt8163-radar-puffin.dtb) ;;
  qemu)   out=$root/build/kernel-qemu; fragments+=("$here/qemu.config"); targets=(zImage) ;;
  qemu-arm64)
          out=$root/build/kernel-qemu-arm64; fragments+=("$here/qemu-arm64.config"); targets=(Image)
          arch=arm64; cross=aarch64-unknown-linux-gnu-; compat=armv7l-unknown-linux-gnueabihf- ;;
  mt8516) out=$root/build/kernel-mt8516; fragments+=("$here/mt8516.config")
          targets=(Image.gz mediatek/mt8516-amazon-donut.dtb)
          arch=arm64; cross=aarch64-unknown-linux-gnu-; compat=armv7l-unknown-linux-gnueabihf- ;;
  mt8519) out=$root/build/kernel-mt8519; fragments+=("$here/mt8519.config")
          targets=(zImage mediatek/cannoli-hvt.dtb mediatek/cannoli-dvt-plastic.dtb) ;;
  x86)    out=$root/build/kernel-x86; fragments+=("$here/x86.config"); targets=(bzImage)
          arch=x86_64; cross= ;;
  *) echo "unknown variant: $variant" >&2; exit 2 ;;
esac
[ -z "${DEBUG:-}" ] || fragments+=("$here/debug.config")
debs=$root/build/debs
mkdir -p "$out" "$debs"

log() { printf '\033[1m==> %s\033[0m\n' "$*"; }

# --- source ---------------------------------------------------------------
# checkout_pinned DIR REPO REF COMMIT: DIR at exactly COMMIT, fetched (shallow)
# from REPO's REF if it is not there yet, clean.
checkout_pinned() {
  local dir=$1 repo=$2 ref=$3 commit=$4
  [ -d "$dir/.git" ] || git init -q "$dir"
  if ! git -C "$dir" cat-file -e "$commit^{commit}" 2>/dev/null; then
    log "fetching $ref from $repo"
    git -C "$dir" fetch -q --depth 1 "$repo" "$ref"
    git -C "$dir" cat-file -e "$commit^{commit}" 2>/dev/null ||
      { echo "$repo $ref is not $commit (see kernel/source.env)" >&2; exit 1; }
  fi
  git -C "$dir" checkout -q --force --detach "$commit"
  git -C "$dir" clean -qfdx
}

checkout_pinned "$deb" "$DEBIAN_REPO" "refs/tags/$DEBIAN_TAG" "$DEBIAN_BASE"
checkout_pinned "$src" "${LINUX_LOCAL:-$LINUX_REPO}" "refs/tags/$LINUX_TAG" "$LINUX_BASE"

series=$deb/debian/patches/series
log "applying Debian $DEBIAN_TAG ($(grep -cvE '^[[:space:]]*(#|$)' "$series") patches) + $(ls "$here"/patches/*.patch | wc -l) Pinna patches"
while read -r p _; do
  case "$p" in ''|'#'*) continue ;; esac
  git -C "$src" apply --whitespace=nowarn "$deb/debian/patches/$p"
done < "$series"
for p in "$here"/patches/*.patch; do
  git -C "$src" apply --whitespace=nowarn "$p"
done

# --- config ---------------------------------------------------------------
# LOCALVERSION= (set, empty): the release is exactly <version>-pinna, no "+"
# for the patched tree.
mk() { make -C "$src" O="$out" ARCH="$arch" ${cross:+CROSS_COMPILE="$cross"} ${compat:+CROSS_COMPILE_COMPAT="$compat"} LOCALVERSION= "$@"; }

log "config ($variant): allnoconfig + ${fragments[*]##*/}"
cat "${fragments[@]}" > "$out/fragments.config"
mk -s KCONFIG_ALLCONFIG="$out/fragments.config" allnoconfig 2>&1 | grep -v 'override: reassigning to symbol' || true

# Every symbol the fragments ask for must have landed (later fragments win).
fail=0
while IFS= read -r line; do
  case "$line" in
    CONFIG_*=*)
      grep -qxF "$line" "$out/.config" || { echo "missing: $line (got: $(grep "^${line%%=*}=" "$out/.config" || echo unset))"; fail=1; } ;;
    "# CONFIG_"*" is not set")
      sym=${line#\# }; sym=${sym%% *}
      if grep -q "^$sym=" "$out/.config"; then echo "should be unset: $sym"; fail=1; fi ;;
  esac
done < <(cat "${fragments[@]}" | awk '
  /^CONFIG_[A-Za-z0-9_]+=/ { k=$0; sub(/=.*/, "", k); v[k]=$0; if (!(k in o)) { o[k]=n++ } ; next }
  /^# CONFIG_[A-Za-z0-9_]+ is not set/ { k=$2; v[k]=$0; if (!(k in o)) { o[k]=n++ } }
  END { for (k in o) print v[k] }')
[ "$fail" = 0 ] || { echo "kernel config assertions failed" >&2; exit 1; }
log "config assertions passed"
if [ -n "${CHECK_ONLY:-}" ]; then
  exit 0
fi

# --- build ----------------------------------------------------------------
if [ "$variant" = device ]; then
  # Built into the kernel (CONFIG_EXTRA_FIRMWARE): the SPI microphone driver
  # loads it into the FPGA at probe. Amazon's file, fetched, never committed
  # (scripts/fetch-firmware.sh, ADR 0006).
  fw=$root/blobs/firmware/i2s_to_spi_v34.bin
  [ -f "$fw" ] || { echo "$fw missing: run just firmware" >&2; exit 1; }
  install -Dm0644 "$fw" "$src/firmware/i2s_to_spi_v34.bin"
fi

log "building ${targets[*]}"
mk -s -j"$(nproc)" "${targets[@]}"

if [ "$variant" = device ]; then
  # The decompressed kernel must end below the ATF carve-out at 0x43000000.
  end_virt=$(${cross}nm "$out/vmlinux" | awk '$3=="_end"{print $1}')
  end_phys=$(( 0x$end_virt - 0xC0000000 + 0x40000000 ))
  printf 'vmlinux _end: virt 0x%s phys 0x%x (limit 0x43000000)\n' "$end_virt" "$end_phys"
  [ "$end_phys" -lt $((0x43000000)) ] || { echo "kernel image overlaps ATF" >&2; exit 1; }
fi

if [ "$variant" = mt8516 ]; then
  # LK gunzips the Image to 0x40080000; with its BSS (the header's
  # image_size) it must end below BL31 at 0x43000000 (lidar-mainline,
  # tools/lidarboot.py).
  size=$(od -An -t u8 -j 16 -N 8 "$out/arch/arm64/boot/Image" | tr -d ' ')
  end=$(( 0x40080000 + size ))
  printf 'Image end: 0x%x (limit 0x43000000)\n' "$end"
  [ "$end" -lt $((0x43000000)) ] || { echo "kernel image overlaps BL31" >&2; exit 1; }
fi

krel=$(cat "$out/include/config/kernel.release")
log "kernel release $krel"
if [ "$variant" != device ]; then
  log "done: $out/arch/${arch%_64}/boot/${targets[0]}"
  exit 0
fi

# --- Debian package ---------------------------------------------------------
pkgroot=$out/deb
rm -rf "${pkgroot:?}"
mkdir -p "$pkgroot/DEBIAN" "$pkgroot/boot" "$pkgroot/usr/lib/linux-image-$krel"
install -m0644 "$out/arch/arm/boot/zImage" "$pkgroot/boot/vmlinuz-$krel"
install -m0644 "$out/.config" "$pkgroot/boot/config-$krel"
install -m0644 "$out/System.map" "$pkgroot/boot/System.map-$krel"
# Device trees built from the tree. The boot image uses mt8163-radar-puffin.dtb
# from the build directory (see the justfile's bootimg).
find "$out/arch/arm/boot/dts/mediatek" -maxdepth 1 -name 'mt8163-*.dtb' -exec install -m0644 {} "$pkgroot/usr/lib/linux-image-$krel/" \;
version=$(git -C "$root" log -1 --format=%h -- kernel)
cat > "$pkgroot/DEBIAN/control" <<EOF
Package: linux-image-$krel
Version: $krel-1+g$version
Architecture: armhf
Maintainer: Pinna <pinna@localhost>
Section: kernel
Priority: optional
Provides: linux-image
Description: Pinna kernel for the Amazon Echo (2nd gen) family (MT8163)
 Linux $krel from Debian's linux $DEBIAN_TAG source with the MT8163 board
 support and fixes in kernel/patches.
EOF
rm -f "$debs"/linux-image-*.deb
dpkg-deb --root-owner-group -Zxz --build "$pkgroot" "$debs/linux-image-${krel}_armhf.deb" >/dev/null
ls -l "$debs"/linux-image-*.deb
log "done"
