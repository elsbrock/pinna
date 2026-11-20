# ADR 0001: Platform — Debian image, own kernel command line

Status: accepted (2026-11-15). Boot path amended by ADR 0003: the boot image
now carries an initramfs (pinna-init) that picks the A/B slot and mounts the
root through dm-verity; the forced command line stays. Slot choice superseded
by ADR 0019: the bootloader picks the slot, pinna-init boots its root. Kernel superseded by
ADR 0006: Debian's 6.12 with the MT8163 board support ported in
`kernel/patches`, config from allnoconfig + fragments.

## Context

Pinna runs on the Echo (2nd gen) family (MT8163, ARMv7, ~480 MB RAM). It
starts from LibreEcho, which runs everything from a BusyBox initramfs with
hand-written C daemons. We want a modern, standard userland and an easy build.

Device facts that constrain the design:

- *(Superseded by ADR 0019: that was a bug in Kaeru before `ab93750`; a fixed
  Kaeru loads the boot partition of the slot its record names.)*
  Amazon LK, unlocked with Kaeru/amonet, **always loads `boot_a`**; its A/B
  slot record does not change which image boots on the Dot.
- LK appends `root=/dev/dm-0 dm=… skip_initramfs` to the command line. No
  kernel code consumes LK's other arguments (`androidboot.*`, `bootopt`).
- MT8163 board support (pinctrl, clocks, audio, connectivity, SPI-FPGA mics)
  exists only in LibreEcho's 6.1 Linux tree, not upstream.
- `fastboot` (Volume Down + plug in) is always available for recovery.

## Decision

- **Userland:** Debian trixie armhf built with mkosi; read-only **erofs** root
  on `system_a` (since ADR 0003: signed A/B slots `system_a`/`system_b`),
  persistent `/var` (ext4, `x-systemd.makefs`) on `userdata`. systemd,
  networkd, resolved (mDNS), wpa_supplicant, OpenSSH.
- **Kernel:** LibreEcho's Linux tree (release 0.14.5) + patches in `kernel/patches`,
  plus `kernel/pinna.config` (since ADR 0006: `kernel/common.config` and a
  fragment per variant); packaged as a `.deb`. `CONFIG_CMDLINE_FORCE` sets
  our own command line (`root=PARTLABEL=system_a rootfstype=erofs …`), so LK's
  injected root is ignored. Originally no initramfs: the boot image carried a
  placeholder cpio without `/init` so LK still saw a ramdisk. Since ADR 0003
  the ramdisk is the pinna-init initramfs (slot choice, dm-verity).
- **Boot image:** Android v0, built by `pinna-bootimg` (byte-identical
  round-trip verified). The DTB is `mt8163-radar-puffin.dtb` from our own
  kernel build: the Echo Dot (2nd gen) boots the same device tree as the Echo
  (2nd gen). Amended 2026-11-15: until then the boot image used a DTB extracted
  from LibreEcho's original `boot_a`; the kernel-built one
  decompiles (`dtc -I dtb -O dts`) to the identical tree, so the build no
  longer needs a file from another firmware.
- **Safety net:** a one-shot `bootonce-bootloader` in `misc` armed early each
  boot and cleared once healthy; since ADR 0003 not during the trial boot of
  a new slot that has a good fallback. Amended 2026-11-17: first planned for
  dev images only, it is armed in every image (`/etc/pinna/fastboot-guard`,
  ADR 0003).

## Consequences

- *(Superseded by ADR 0019: kernel and root share a slot, and the bootloader
  chooses it.)* Kernel updates are separate from root updates. A/B for the
  root (`system_a`/`system_b`) with dm-verity: ADR 0003 (the initramfs chooses
  the slot, so the command line stays fixed).
- Wi-Fi/BT firmware and the microphone FPGA bitstream (ADR 0006) are
  fetched, not committed: `just firmware` downloads them from
  `https://static.iodev.org/pinna/firmware/` into `blobs/firmware/` and checks
  their sha256, and the kernel and image builds take them from there. Neither
  has a source other than the device's stock firmware or LibreEcho's Linux tree, so an
  owner without them would have no Wi-Fi or no microphones. They are Amazon's
  files and carry no license, so they stay out of the repository and its
  history: a claim against them concerns one download, not the source tree.
  Amended 2026-11-20; from 2026-11-15 they were committed (`firmware/`,
  `kernel/firmware/`), and before that owner-supplied in `blobs/`.
- Moving to Debian's kernel source (6.12 LTS) means porting the board patches:
  done in ADR 0006.
- arm64 may be possible (ATF runs 64-bit, `bootopt=64S3,…`); not pursued yet.
