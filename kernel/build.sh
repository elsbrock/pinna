#!/usr/bin/env bash
# Build the Pinna kernel for the Echo (2nd gen) family and package it as a
# Debian package.
#
#   kernel/build.sh            -> build/kernel/{zImage,vmlinux,.config} + build/debs/linux-image-*.deb
#   kernel/build.sh qemu       -> build/kernel-qemu/ (ARM QEMU virt variant for boot tests, no package)
#   kernel/build.sh x86        -> build/kernel-x86/ (x86_64 QEMU/KVM variant for smoke tests, no package)
#
# Requires the dev shell (cross gcc 15, make, dtc, dpkg).
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(dirname "$here")
# shellcheck source=source.env
source "$here/source.env"

variant=${1:-device}
src=$root/build/linux-src
arch=arm
cross=armv7l-unknown-linux-gnueabihf-
case "$variant" in
  device) out=$root/build/kernel; defconfig=$LINUX_DEFCONFIG; fragments=("$here/pinna.config"); targets=(zImage dtbs) ;;
  qemu)   out=$root/build/kernel-qemu; defconfig=multi_v7_defconfig; fragments=("$here/pinna.config" "$here/qemu.config"); targets=(zImage) ;;
  x86)    out=$root/build/kernel-x86; defconfig=x86_64_defconfig; fragments=("$here/pinna.config" "$here/x86.config"); targets=(bzImage)
          arch=x86_64; cross= ;;
  *) echo "unknown variant: $variant" >&2; exit 2 ;;
esac
debs=$root/build/debs
mkdir -p "$out" "$debs"

log() { printf '\033[1m==> %s\033[0m\n' "$*"; }

# --- source ---------------------------------------------------------------
if [ ! -d "$src/.git" ]; then
  if [ -d "$LINUX_LOCAL/.git" ] && git -C "$LINUX_LOCAL" cat-file -e "$LINUX_BASE^{commit}" 2>/dev/null; then
    log "cloning kernel source (shared objects from $LINUX_LOCAL)"
    git clone -q --shared --no-checkout "$LINUX_LOCAL" "$src"
  else
    log "cloning kernel source from $LINUX_REPO"
    git clone -q --filter=blob:none --no-checkout "$LINUX_REPO" "$src"
  fi
fi
log "checking out $LINUX_BASE + $(ls "$here"/patches/*.patch | wc -l) patches"
git -C "$src" checkout -q --force --detach "$LINUX_BASE"
git -C "$src" clean -qfdx
for p in "$here"/patches/*.patch; do
  git -C "$src" apply --whitespace=nowarn "$p"
done
# Release string is exactly <version>-pinna (no "+" for the patched tree).
: > "$src/.scmversion"

# --- config ---------------------------------------------------------------
mk() { make -C "$src" O="$out" ARCH="$arch" ${cross:+CROSS_COMPILE="$cross"} "$@"; }

log "config ($variant): $defconfig + ${fragments[*]##*/}"
mk -s "$defconfig"
ARCH="$arch" CROSS_COMPILE="$cross" "$src/scripts/kconfig/merge_config.sh" -m -O "$out" "$out/.config" "${fragments[@]}" >/dev/null
mk -s olddefconfig

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
log "building ${targets[*]}"
mk -s -j"$(nproc)" "${targets[@]}"

if [ "$variant" = device ]; then
  # The decompressed kernel must end below the ATF carve-out at 0x43000000.
  end_virt=$(${cross}nm "$out/vmlinux" | awk '$3=="_end"{print $1}')
  end_phys=$(( 0x$end_virt - 0xC0000000 + 0x40000000 ))
  printf 'vmlinux _end: virt 0x%s phys 0x%x (limit 0x43000000)\n' "$end_virt" "$end_phys"
  [ "$end_phys" -lt $((0x43000000)) ] || { echo "kernel image overlaps ATF" >&2; exit 1; }
fi

krel=$(cat "$out/include/config/kernel.release")
log "kernel release $krel"
if [ "$variant" != device ]; then
  log "done: $out/arch/$([ "$arch" = x86_64 ] && echo x86/boot/bzImage || echo arm/boot/zImage)"
  exit 0
fi

# --- Debian package ---------------------------------------------------------
pkgroot=$out/deb
rm -rf "${pkgroot:?}"
mkdir -p "$pkgroot/DEBIAN" "$pkgroot/boot" "$pkgroot/usr/lib/linux-image-$krel"
install -m0644 "$out/arch/arm/boot/zImage" "$pkgroot/boot/vmlinuz-$krel"
install -m0644 "$out/.config" "$pkgroot/boot/config-$krel"
install -m0644 "$out/System.map" "$pkgroot/boot/System.map-$krel"
# Device trees built from the tree. The boot image uses libreecho-radar-puffin.dtb
# from the build directory (see the justfile's bootimg).
find "$out/arch/arm/boot/dts" -maxdepth 1 -name 'libreecho-*.dtb' -exec install -m0644 {} "$pkgroot/usr/lib/linux-image-$krel/" \;
version=$(git -C "$src" rev-parse --short=10 HEAD)
cat > "$pkgroot/DEBIAN/control" <<EOF
Package: linux-image-$krel
Version: $krel-1+g$version
Architecture: armhf
Maintainer: Pinna <pinna@localhost>
Section: kernel
Priority: optional
Provides: linux-image
Description: Pinna kernel for the Amazon Echo (2nd gen) family (MT8163)
 Linux $krel with MT8163 board support, built from LibreEcho-Linux
 release/0.14.5 plus the patches in kernel/patches.
EOF
rm -f "$debs"/linux-image-*.deb
dpkg-deb --root-owner-group -Zxz --build "$pkgroot" "$debs/linux-image-${krel}_armhf.deb" >/dev/null
ls -l "$debs"/linux-image-*.deb
log "done"
