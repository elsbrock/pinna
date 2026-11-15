# Shared helpers for the VM tests (sourced).
#
# The test disk mirrors the device's partitions by GPT name:
#   misc (512 KiB) | expdb (1 MiB) | system_a (768 MiB) | system_b (768 MiB) | userdata (512 MiB)
# system_a holds a sealed slot image, system_b starts empty; userdata is left
# unformatted (first boot formats it) unless the caller fills it.

vm_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

log() { printf '\033[1m==> %s\033[0m\n' "$*"; }
status=0
fail() { printf '\033[31mFAIL: %s\033[0m\n' "$*"; status=1; }
pass() { printf '\033[32mok\033[0m   %s\n' "$*"; }

# Partition by GPT name -> "first last" sector.
part_range() {
  local disk=$1 n
  n=$(sgdisk -p "$disk" | awk -v name="$2" '$NF==name {print $1}')
  [ -n "$n" ] || { echo "no partition $2 in $disk" >&2; return 1; }
  sgdisk -i "$n" "$disk" | awk '/First sector/{f=$3} /Last sector/{l=$3} END{print f, l}'
}

# Write FILE into partition NAME of DISK (sparse, in place).
part_write() {
  local disk=$1 name=$2 file=$3 first last
  read -r first last <<<"$(part_range "$disk" "$name")"
  local size=$(( (last - first + 1) * 512 ))
  [ "$(stat -c %s "$file")" -le "$size" ] || { echo "$file does not fit in $name" >&2; return 1; }
  dd if="$file" of="$disk" bs=4M oflag=seek_bytes seek=$((first * 512)) conv=notrunc,sparse status=none
}

# Copy partition NAME of DISK out to FILE.
part_read() {
  local disk=$1 name=$2 file=$3 first last
  read -r first last <<<"$(part_range "$disk" "$name")"
  dd if="$disk" of="$file" bs=4M iflag=skip_bytes,count_bytes skip=$((first * 512)) \
     count=$(( (last - first + 1) * 512 )) conv=sparse status=none
}

# make_disk DISK SLOT_IMAGE: fresh test disk with SLOT_IMAGE in system_a and a
# fake Kaeru LK payload header in expdb, so the fastboot guard arms like on
# the device (MTK image magic, then the image name "LK").
make_disk() {
  local disk=$1 image=$2 expdb_first
  rm -f "$disk"
  truncate -s 2056M "$disk"
  sgdisk -a 2048 \
    -n 1:0:+512K -c 1:misc \
    -n 2:0:+1M -c 2:expdb \
    -n 3:0:+768M -c 3:system_a \
    -n 4:0:+768M -c 4:system_b \
    -n 5:0:+512M -c 5:userdata \
    "$disk" >/dev/null
  part_write "$disk" system_a "$image"
  read -r expdb_first _ <<<"$(part_range "$disk" expdb)"
  printf '\x88\x16\x88\x58\x00\x00\x00\x00LK\x00' |
    dd of="$disk" bs=512 seek="$expdb_first" conv=notrunc status=none
}

# The VM's journal, read off its userdata partition after power-off:
# extract_journal DISK WORKDIR -> WORKDIR/journal/ (journalctl --directory).
extract_journal() {
  local disk=$1 work=$2
  part_read "$disk" userdata "$work/userdata.ext4"
  # The VM may have been killed, not shut down: replay ext4's own journal on the copy.
  e2fsck -fy "$work/userdata.ext4" >/dev/null 2>&1 || true
  rm -rf "${work:?}/journal"
  mkdir -p "$work/journal"
  debugfs -c -R "rdump /log/journal $work/journal" "$work/userdata.ext4" >/dev/null 2>&1 || true
}

# pinna-bootctl slot-status (host build) on the misc and slot partitions of DISK.
slot_status() {
  local disk=$1 work=$2 p
  mkdir -p "$work/parts"
  for p in misc system_a system_b; do part_read "$disk" "$p" "$work/parts/$p"; done
  cargo run -q --manifest-path "$vm_root/Cargo.toml" -p pinna-bootctl -- \
    --misc "$work/parts/misc" --partdir "$work/parts" --run-dir /nonexistent slot-status
}

# qemu_start ARCH KERNEL INITRD DISK CONSOLE PIDFILE NETDEV
qemu_start() {
  local arch=$1 kernel=$2 initrd=$3 disk=$4 console=$5 pidfile=$6 netdev=$7
  if [ "$arch" = armhf ]; then
    qemu-system-arm \
      -M virt,highmem=off -cpu max -smp 2 -m 512 \
      -kernel "$kernel" -initrd "$initrd" \
      -drive if=none,file="$disk",format=raw,id=hd0 \
      -device virtio-blk-device,drive=hd0 \
      -netdev "$netdev" -device virtio-net-device,netdev=n0 \
      -device virtio-rng-device \
      -display none -serial file:"$console" \
      -pidfile "$pidfile" -daemonize
  else
    local accel=tcg
    [ -w /dev/kvm ] && accel=kvm
    qemu-system-x86_64 \
      -M q35,accel="$accel" -cpu max -smp 2 -m 1024 \
      -kernel "$kernel" -initrd "$initrd" \
      -drive if=none,file="$disk",format=raw,id=hd0 \
      -device virtio-blk-pci,drive=hd0 \
      -netdev "$netdev" -device virtio-net-pci,netdev=n0 \
      -device virtio-rng-pci \
      -display none -serial file:"$console" \
      -pidfile "$pidfile" -daemonize
  fi
}

console_text() { sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' "$1" 2>/dev/null; }
