# ADR 0001: Platform — Debian image, own kernel command line

Status: accepted (2026-11-15). Boot path amended by ADR 0003: the boot image
now carries an initramfs (pinna-init) that picks the A/B slot and mounts the
root through dm-verity; the forced command line stays.

## Context

Pinna replaces LibreEcho on the Echo (2nd gen) family (MT8163, ARMv7, ~480 MB
RAM). LibreEcho runs everything from a BusyBox initramfs with hand-written C
daemons. We want a modern, standard userland and an easy build.

Device facts that constrain the design:

- Amazon LK, unlocked with Kaeru/amonet, **always loads `boot_a`**; its A/B
  slot record does not change which image boots on the Dot.
- LK appends `root=/dev/dm-0 dm=… skip_initramfs` to the command line. No
  kernel code consumes LK's other arguments (`androidboot.*`, `bootopt`).
- MT8163 board support (pinctrl, clocks, audio, connectivity, SPI-FPGA mics)
  exists only in LibreEcho-Linux 6.1, not upstream.
- `fastboot` (Volume Down + plug in) is always available for recovery.

## Decision

- **Userland:** Debian trixie armhf built with mkosi; read-only **erofs** root
  on `system_a` (since ADR 0003: signed A/B slots `system_a`/`system_b`),
  persistent `/var` (ext4, `x-systemd.makefs`) on `userdata`. systemd,
  networkd, resolved (mDNS), wpa_supplicant, OpenSSH.
- **Kernel:** LibreEcho-Linux release/0.14.5 + patches in `kernel/patches`,
  plus `kernel/pinna.config`; packaged as a `.deb`. `CONFIG_CMDLINE_FORCE` sets
  our own command line (`root=PARTLABEL=system_a rootfstype=erofs …`), so LK's
  injected root is ignored. Originally no initramfs: the boot image carried a
  placeholder cpio without `/init` so LK still saw a ramdisk. Since ADR 0003
  the ramdisk is the pinna-init initramfs (slot choice, dm-verity).
- **Boot image:** Android v0, built by `pinna-bootimg` with the known-good DTB
  from the device's original `boot_a` (byte-identical round-trip verified).
- **Safety net:** a one-shot `bootonce-bootloader` in `misc` armed early each
  boot and cleared once healthy (dev images); since ADR 0003 not during the
  trial boot of a new slot that has a good fallback.

## Consequences

- Kernel updates are separate from root updates. A/B for the root
  (`system_a`/`system_b`) with dm-verity: ADR 0003 (the initramfs chooses the
  slot, so the command line stays fixed).
- Wi-Fi/BT firmware is owner-supplied (`blobs/`), never committed.
- Moving to Debian's kernel source (6.12 LTS) means porting the board patches;
  tracked separately.
- arm64 may be possible (ATF runs 64-bit, `bootopt=64S3,…`); not pursued yet.
