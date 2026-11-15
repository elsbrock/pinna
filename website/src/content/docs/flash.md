---
title: Flash the device
description: What gets written to the Echo Dot, how to flash it over fastboot, and how to verify the first boot.
---

This walks an Echo Dot that currently runs LibreEcho to Pinna. Every step below
says what it writes and how to undo it; nothing else on the device is touched.

## What gets written

| Partition | Before | After |
| --- | --- | --- |
| `boot_a` (p10) | LibreEcho boot image | Pinna boot image (`build/pinna-boot.img`) — the slot LK always loads |
| `boot_b` (p11) | LibreEcho copy | Pinna boot image (manual fallback copy) |
| `system_a` (p13) | Fire OS system (LibreEcho reads Wi-Fi firmware from it) | Pinna erofs root (`mkosi/mkosi.output/pinna.root-arm.raw`) |
| `userdata` (p16) | LibreEcho `/data` | Wiped; Pinna formats it as `/var` on first boot |

Never written: preloader and boot areas, `lk_a`/`lk_b`, `tee1`/`tee2`,
`expdb` (the Kaeru payload), `persist` (identity, calibration, MAC) and `misc`
(only the bootloader command field, at runtime, by the fastboot guard).

## 0. Back up first

Take a full backup of the device before the first write — `mmcblk0.img` plus
per-partition images with checksums. The runbook in the repository
(`docs/runbook-first-flash.md`) documents the exact layout used for the
development device, and the restore script expects that layout.

## 1. Enter fastboot

Unplug the Dot, hold **Volume Down**, plug the micro-USB cable in, and release
when the ring changes.

```sh
fastboot devices
scripts/flash-dot.sh check    # product must be BISCUIT
```

## 2. Flash the root filesystem

```sh
scripts/flash-dot.sh root     # system_a <- erofs root, userdata wiped
```

If LK rejects the ~150 MB raw image (`max-download-size`), convert it to a
sparse image first and point the script at it:

```sh
img2simg mkosi/mkosi.output/pinna.root-arm.raw pinna.root.simg
ROOT_IMG=pinna.root.simg scripts/flash-dot.sh root
```

## 3. Try the kernel from RAM (writes nothing)

```sh
scripts/flash-dot.sh try      # fastboot boot build/pinna-boot.img
```

If Kaeru does not support `fastboot boot`, skip to the next step; the recovery
path is the same.

Within about 20 seconds the host should see a new USB device, **"Pinna (Echo
Dot 2nd gen)"**:

- serial console: `/dev/ttyACM0` (root autologin) — `picocom -b 115200 /dev/ttyACM0`
- network: the host gets a DHCP lease in `10.42.0.x`; `ssh root@10.42.0.1` (or `root@pinna.local`)

Check the essentials:

```sh
pinnactl status
systemctl --failed
journalctl -b -u pinna-wifi -u wpa_supplicant@wlan0 -u pinnad
ip addr show wlan0
pinna-bootctl status
pinnactl led-test             # ring animation
pinnactl volume 40
```

## 4. Make it persistent

From Pinna: `pinna-bootctl reboot-bootloader` (or Volume Down + replug), then

```sh
scripts/flash-dot.sh boot     # boot_a + boot_b <- Pinna, reboot
```

## 5. Adopt it in Home Assistant

Pinna advertises `_esphomelib._tcp` via mDNS. Home Assistant should offer
"Echo Dot" as a discovered ESPHome device. Add it, then point a voice assistant
pipeline at it and say "Alexa…". See [Home Assistant](/docs/home-assistant/).

## Things that may need tuning

Mic gain, the wake word threshold, the speaker mixer sequence and LED
direction are not testable off-device. Defaults live in
`/etc/pinna/pinnad.toml`; `journalctl -u pinnad` logs wake word scores.

To undo everything, see [Recovery](/docs/recovery/).
