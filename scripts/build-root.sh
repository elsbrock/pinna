#!/usr/bin/env bash
# Build the erofs root with mkosi and seal it into a signed slot image
# (erofs + footer + dm-verity hash tree, see docs/adr/0003-updates.md).
#
#   scripts/build-root.sh                  # armhf  -> build/pinna.root.img
#   scripts/build-root.sh --arch x86_64    # x86_64 -> build/pinna-x86.root.img
#
#   --version V        image version (default: $PINNA_VERSION, else
#                      YYYY.MM.DD.HHMM-<git short hash>)
#   --out FILE         sealed slot image
#   --output-dir DIR   mkosi output directory (default: mkosi/mkosi.output)
#   --extra-tree DIR   one more tree on top of the configured ones (tests only)
#
# The version is ImageVersion= for mkosi (IMAGE_VERSION in the image's
# os-release) and the version in the signed footer; this checks they agree.
# secrets/machine-id is optional: without it mkosi writes `uninitialized` and
# pinna-init sets a machine id per device (ADR 0015).
# Needs: the assembled extra tree (scripts/assemble-rootfs-extra.sh) and the
# signing key secrets/release.key (`just keygen`).
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
arch=armv7
version=
out=
outdir=$root/mkosi/mkosi.output
extra=()
while [ $# -gt 0 ]; do
  case "$1" in
    --arch) arch=${2:?}; shift 2 ;;
    --version) version=${2:?}; shift 2 ;;
    --out) out=$(realpath -m "${2:?}"); shift 2 ;;
    --output-dir) outdir=$(realpath -m "${2:?}"); shift 2 ;;
    --extra-tree) extra+=("--extra-tree=$(realpath "${2:?}")"); shift 2 ;;
    *) sed -n '2,19p' "$0"; exit 2 ;;
  esac
done
[ -s "$root/secrets/release.key" ] || { echo "no secrets/release.key (run: just keygen)" >&2; exit 1; }

if [ "$arch" = x86_64 ]; then
  mkosi_arch=(--architecture x86-64)
  raw=pinna-x86.root-x86-64.raw
  out=${out:-$root/build/pinna-x86.root.img}
else
  mkosi_arch=()
  raw=pinna.root-arm.raw
  out=${out:-$root/build/pinna.root.img}
fi
version=${version:-${PINNA_VERSION:-"$(date +%Y.%m.%d.%H%M)-$(git -C "$root" rev-parse --short HEAD)"}}

# A release image carries no machine id: each Dot derives its own at boot (ADR 0015).
machine_id=()
if [ -s "$root/secrets/machine-id" ]; then
  [ "${PINNA_RELEASE:-}" != 1 ] || { echo "PINNA_RELEASE=1: secrets/machine-id must not exist (each Dot derives its own, ADR 0015)" >&2; exit 1; }
  machine_id=(--machine-id "$(cat "$root/secrets/machine-id")")
fi

# mkosi bootstraps its Debian tools tree with the host's keyring; NixOS has none in /usr/share.
install -Dm0644 -t "$root/mkosi/mkosi.sandbox/usr/share/keyrings" "$PINNA_DEBIAN_KEYRINGS"/*.gpg
install -Dm0644 "$root/mkosi/apt-sources.sources" "$root/mkosi/mkosi.sandbox/etc/apt/sources.list.d/mkosi.sources"
mkdir -p "$outdir" "$(dirname "$out")"
(
  cd "$root/mkosi"
  mkosi "${mkosi_arch[@]}" "${machine_id[@]}" \
    --image-version "$version" --output-directory "$outdir" "${extra[@]}" -f build
)

got=$(dump.erofs --path=/usr/lib/os-release --cat "$outdir/$raw" | sed -n 's/^IMAGE_VERSION=//p' | tr -d '"')
[ "$got" = "$version" ] || { echo "os-release IMAGE_VERSION is '$got', expected '$version'" >&2; exit 1; }
cargo run -q --release --manifest-path "$root/Cargo.toml" -p pinna-image -- \
  seal --key "$root/secrets/release.key" --version "$version" "$outdir/$raw" "$out"
