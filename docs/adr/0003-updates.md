# ADR 0003: Updates — signed A/B root slots, dm-verity, pinna-init

Status: accepted (2026-11-15). Amends ADR 0001 (the boot image now carries an
initramfs). Superseded by ADR 0019, where kernel and root share a slot in the
bootloader's slot record: the slot record at `0x40000` and its try counting,
the rollback, "boot_a is not A/B" and "LK always loads boot_a" (so kernel
updates are no longer a manual fastboot flash, and the manifest gains
`boot`), pinna-init choosing the slot and falling back to the other root in
the same boot (it now boots the bootloader's slot, and retires a failed one
before restarting), LK's record at 0x360 (Kaeru's, 7 bytes), the reason given
against systemd-sysupdate, and clearing `system_b`
to make slot a boot (`set_active a` does that now). The boot guard's target
changes once the recovery image exists (ADR 0019).

## Context

- LK always loads `boot_a`; there is one kernel slot. It finds partitions by
  GPT name, so names cannot change. `system_a`/`system_b` (768 MiB each) are
  free for two root filesystems.
- `misc` (512 KiB): LK uses the bootloader message at 0 and 5 bytes at 0x360;
  from 0x1000 up it is free and zero.
- The kernel has BLK_DEV_INITRD, DM_VERITY, EROFS, SHA256, no modules, and
  forces its own command line.

## Decision

**Slot image** (what goes into `system_a`/`system_b`), built by
`scripts/build-root.sh` from mkosi's erofs and sealed by `pinna-image seal`:

```text
0              data_size          data_size + 4096
[ erofs root  ][ footer (4 KiB)  ][ dm-verity hash tree ]
```

`data_size` = erofs `blocks << blkszbits` from the superblock at 1024 (magic
0xE0F5E1E2 at +0, blkszbits at +12, blocks at +36; 4 KiB blocks required). The
hash tree is dm-verity format 1 without a verity superblock, sha256, 4 KiB
blocks, a random salt per image — byte-identical to
`veritysetup format --no-superblock` (tested). A sealed slot is still a plain
erofs at offset 0.

| Footer offset | Size | Field |
|---|---|---|
| 0x00 | 8 | magic `PINNAFTR` |
| 0x08 | 4 | format version (1), then 4 reserved |
| 0x10 | 8 | data_size (= footer offset) |
| 0x18 | 8 | hash_offset (= data_size + 4096) |
| 0x20 | 8 | hash_size |
| 0x28 | 4+4 | data / hash block size (4096) |
| 0x30 | 16 | hash algorithm `sha256` |
| 0x40 | 32 | salt |
| 0x60 | 32 | root hash |
| 0x80 | 64 | image version (= `IMAGE_VERSION` in os-release, `ImageVersion=`) |
| 0xc0 | 64 | ed25519 signature over 0x00..0xc0 |

All integers little-endian, rest zero. The signing key is a hex seed in
`secrets/release.key` (never committed, `just keygen`); `keys/release.pub` is
committed and compiled into pinna-init, pinna-bootctl and pinnactl.

**Boot.** The boot image's ramdisk is an initramfs with one static binary,
`pinna-init` (PID 1), and a static busybox for its rescue mode (ADR 0012): mount devtmpfs/proc/sysfs/tmpfs; find `misc`,
`system_a`, `system_b` by `PARTNAME` (up to 15 s); choose the slot from the
state in misc; check that slot's footer signature and the top hash block;
create the read-only dm-verity device `root` through the device-mapper
ioctls; mount it (erofs) on /sysroot and check for `/sbin/init`. If any of
that fails, try the other slot and record the first as unbootable. Then write
`/run/pinna/boot-slot` and `boot-version`, move the API mounts, switch root and
exec `/sbin/init`. Nothing bootable: rescue mode (ADR 0012; it used to be a
reboot loop until fastboot).
Everything goes to /dev/kmsg as `pinna-init: …`. The kernel command line is
unchanged; its `root=` only matters for a boot image without pinna-init, which
mounts `system_a` as plain erofs (no verity, no slot choice; `mark-good` then
has nothing to mark).

**Slot state** at `misc` + 0x40000 (one sector, written with flush, page-cache
drop and readback of the sector and its neighbours):

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `PINNASLT` |
| 8 | 1 | version (1) |
| 9 | 1 | active slot `a`/`b` |
| 10, 11 | 1, 1 | slot a: tries_left, successful (0/1) |
| 12, 13 | 1, 1 | slot b: tries_left, successful |
| 14 | 2 | reserved, zero |
| 16 | 4 | crc32 (IEEE) of bytes 0..16 |

Missing or invalid: slot a, treated as good (first flash; nothing written).
Active slot good: boot it. Not good with tries left: decrement, write, boot it.
Out of tries: switch to the other slot if it is good (write), else try it
anyway.

**Health check and rollback.** `pinna-boot-ok.service` waits for pinnad's API
(`pinnactl status`) and sshd, then `pinna-bootctl mark-good` (booted slot
successful) and clears the fastboot guard. `FailureAction=reboot` plus
`JobTimeoutSec=5min`/`JobTimeoutAction=reboot`: a failed check, or no verdict
within 5 minutes of boot, reboots. A trial slot thus counts down its tries and
pinna-init rolls back to the previous slot.

**Fastboot guard** (every image carries `/etc/pinna/fastboot-guard`; it acts
only when the Kaeru payload is in `expdb`): armed only when the booted slot is already
good or there is no good fallback. During the trial boot of a new slot with a
good fallback it is not armed; a failed trial rolls back instead of landing in
fastboot.

**Updater.** `pinnactl update <manifest> [--tries N] [--reboot]` (library
`pinna-update`), manifest `{ "version", "root": { "url", "size", "sha256" } }`
from `http(s)://`, `file://` or a path, `root.url` relative to the manifest.
It refuses while the booted slot is still on trial (the other slot is its only
fallback), marks the target slot unbootable, streams the image into it
(refusing a partition that is in use; size and sha256 checked on the way),
reads it back (signature, version, whole hash tree recomputed), then makes it
active with N tries (default 3). `just release` writes
`build/release/{pinna-<version>.root.img, pinna-boot.img, manifest.json}`.

## Why not systemd-sysupdate

sysupdate keeps versions in GPT partition labels (it renames the target
partition, e.g. `pinna_1.2`) and leaves slot choice and boot counting to the
boot loader (systemd-boot, UKI file names in the ESP). LK finds partitions by
fixed names and always boots `boot_a` without boot counting, so neither half
fits. Choosing the slot in our own initramfs and keeping the state in `misc`
needs no partition renames and no boot loader support.

## Consequences

- `boot_a` (kernel + initramfs) is not A/B: kernel and pinna-init updates are a
  manual `fastboot flash`. Changing the release key means flashing `boot_a`
  and `system_a` together by hand: a new pinna-init rejects slots signed with
  the old key (and stays in rescue mode, ADR 0012), and the running updater only
  accepts images signed with the key it was built with.
- Root images carry ~0.8 % hash tree + 4 KiB. `flash-gen2.sh root` clears the
  head of `system_b` so a fresh flash boots slot a whatever misc says.
- On a slot that is already good, a failed health check also reboots: into
  fastboot with the guard, in a loop without it (no Kaeru payload).
- dm-verity uses its default error mode: a corrupted block reads as EIO.
- Verified on the Echo Dot (2026-11-15): LK boots the pinna-init ramdisk, the
  slot state in `misc` survives reboots, and an update over USB (download,
  write, full read-back verification of a 153 MiB root) takes ~47 s; the new
  slot booted and was marked good.
