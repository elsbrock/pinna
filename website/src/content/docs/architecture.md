---
title: Architecture
description: Boot chain, filesystem layout and the decisions behind Pinna — condensed from the ADRs in the repository.
---

Pinna is deliberately boring infrastructure: a standard Debian userland, a
kernel that does exactly one board bring-up, and one Rust daemon that owns the
device state. Two decision records spell this out in detail —
[ADR 0001 (platform)](https://github.com/elsbrock/pinna/blob/main/docs/adr/0001-platform.md)
and
[ADR 0002 (userland)](https://github.com/elsbrock/pinna/blob/main/docs/adr/0002-userland.md).

## Boot chain

- Amazon's LK always loads **`boot_a`**, whatever the A/B slot record says, so
  that is the slot Pinna targets (and `boot_b` gets the same image as a manual
  fallback).
- LK appends its own `root=` to the kernel command line. Pinna compiles its
  own command line in with `CONFIG_CMDLINE_FORCE`
  (`root=PARTLABEL=system_a rootfstype=erofs …`), which overrides it — and
  means **no initramfs** is needed. The boot image still carries a placeholder
  cpio so LK stays happy.
- The boot image is Android v0: kernel + the device's original known-good DTB,
  packed by `pinna-bootimg`. Round-trips byte-identically against the backup.
- The root filesystem is **erofs, read-only**, on `system_a`. Persistent state
  lives on `userdata`, formatted as ext4 `/var` on first boot
  (`x-systemd.makefs`).

## Userland

- Debian trixie armhf, built by mkosi. systemd owns the machine: networkd,
  resolved (mDNS), wpa_supplicant, OpenSSH, journald, the watchdog and the USB
  gadget (configfs).
- **One daemon** — `pinnad` — runs the state machine: wake word / button →
  Home Assistant Assist run → TTS, plus timers, announcements, mute, the LED
  light entity and volume. `pinna-esphome`, `pinna-wakeword`, `pinna-hal` and
  `pinna-audio` are crates *inside* it, with seams kept for splitting later.
- Home Assistant integration is the **ESPHome native API**; OS-level mDNS
  through systemd-resolved advertises it.
- The **local control API** is ConnectRPC on `127.0.0.1:7470`, unauthenticated
  by design and therefore loopback-only.
- Audio goes straight to **ALSA** on the device for v0 (arecord/aplay). The
  MT8163 sound card has no UCM profile yet, which is why PipeWire is installed
  but unused; laptops and VMs use the PipeWire backend.

## Safety

Dev images arm a one-shot `bootonce-bootloader` request in `misc` on every
boot and clear it once sshd and `pinnad` are healthy. A hung boot followed by
a power cycle therefore lands in fastboot instead of a boot loop. See
[Recovery](/docs/recovery/).

## Open design questions

- **A/B root updates**: `system_a`/`system_b` need a per-slot kernel command
  line; dm-verity would come in via `dm-mod.create=` and a slot-aware boot
  image.
- **Kernel**: board support currently lives in a LibreEcho-Linux 6.1 fork.
  Porting the patches to Debian's 6.12 LTS is tracked separately.
- **arm64**: the ATF runs 64-bit and `bootopt=64S3,…` exists; not pursued yet.
- **Wi-Fi/BT firmware** is owner-supplied (`blobs/`), never committed.
