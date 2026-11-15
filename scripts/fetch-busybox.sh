#!/usr/bin/env bash
# Static busybox for the initramfs' rescue mode (ADR 0012): Debian trixie's
# busybox-static, pinned by version and sha256, extracted to
# build/busybox/busybox-<arch>. Prints that path.
#
#   scripts/fetch-busybox.sh armhf     # device and QEMU armhf
#   scripts/fetch-busybox.sh amd64     # x86_64 VM
#
# When Debian's pool drops this version, move the pin to the current trixie
# busybox-static (both hashes). Needs curl and dpkg-deb (dev shell).
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
version=1.37.0-6+b9
arch=${1:?usage: $0 armhf|amd64}
case "$arch" in
  armhf) sha=8b35210e08d559ecc0ddb660dd96f6f9694f69eb27f1dc608f5eeda56d94509e ;;
  amd64) sha=598a3fd92bdafc34cd81196b2952ad36e910e47a4ecf162f8ce5e8e262598e53 ;;
  *) echo "unknown arch: $arch (armhf or amd64)" >&2; exit 2 ;;
esac
deb=busybox-static_${version}_$arch.deb
dir=$root/build/busybox
out=$dir/busybox-$arch
mkdir -p "$dir"

ok() { echo "$sha  $dir/$deb" | sha256sum -c --quiet - >/dev/null 2>&1; }
if ! ok; then
  curl -fsSL -o "$dir/$deb.part" "https://deb.debian.org/debian/pool/main/b/busybox/$deb"
  mv "$dir/$deb.part" "$dir/$deb"
  ok || { echo "sha256 mismatch for $deb" >&2; exit 1; }
fi
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
dpkg-deb -x "$dir/$deb" "$tmp"
install -m0755 "$tmp/usr/bin/busybox" "$out"
echo "$out"
