#!/usr/bin/env bash
# Development loop: put freshly built Pinna binaries on a running device as a
# systemd-sysext overlay (/var/lib/extensions/pinna-dev), then restart pinnad.
# The image itself stays untouched; `push-dev.sh --remove` drops the overlay.
#
#   PINNA_HOST    ssh destination (default root@pinna.local)
#   PINNA_SSH     ssh command, e.g. "ssh -F ~/.ssh/pinna_config" (default ssh)
#   PINNA_TARGET  Rust target of the binaries (default the Dot's; `just push-vm`
#                 sets the virtual Echo Dot's x86_64 one)
#   PINNA_SYSEXT_DIR  where the overlay goes (default /var/lib/extensions;
#                 `just push-vm` uses /run/extensions, gone when the VM restarts)
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
host=${PINNA_HOST:-root@pinna.local}
read -r -a ssh <<<"${PINNA_SSH:-ssh}"
target=${PINNA_TARGET:-armv7-unknown-linux-musleabihf}
ext=${PINNA_SYSEXT_DIR:-/var/lib/extensions}
name=pinna-dev

remote() { "${ssh[@]}" "$host" "$@"; }

if [ "${1:-}" = --remove ]; then
  remote "rm -rf $ext/$name && systemd-sysext refresh && systemctl restart pinnad"
  echo "removed $name"
  exit 0
fi

stage=$root/build/sysext/$name
rm -rf "${stage:?}"
install -d "$stage/usr/bin" "$stage/usr/lib/extension-release.d"
for bin in pinnad pinnactl pinna-bootctl; do
  install -m0755 "$root/target/$target/release/$bin" "$stage/usr/bin/$bin"
done
# Matches any Debian 13 image; the overlay is for development only.
printf 'ID=debian\nVERSION_ID=13\nSYSEXT_LEVEL=1\n' >"$stage/usr/lib/extension-release.d/extension-release.$name"

echo "==> pushing $name to $host"
tar -C "$root/build/sysext" -cf - "$name" | remote "set -e
  rm -rf $ext/.$name.new
  mkdir -p $ext/.$name.new
  tar -C $ext/.$name.new -xf -
  rm -rf $ext/$name
  mv $ext/.$name.new/$name $ext/$name
  rmdir $ext/.$name.new
  systemd-sysext refresh
  systemctl restart pinnad
  systemd-sysext status --no-pager | sed -n '1,4p'
  pinnad --version"
