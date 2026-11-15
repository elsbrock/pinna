---
title: What you need
description: The device, computer, files and Home Assistant setup that you need before you build and install Pinna.
---

Collect everything on this page before you [build Pinna](/docs/build/). Most
items are files from your own device: Pinna can't include them, because they
belong to the device's manufacturer or to you.

## Device

- **An Echo Dot (2nd gen).** In fastboot, the device reports the product name
  `BISCUIT`. Pinna doesn't support other Echo devices yet.
- **An unlocked bootloader with Kaeru.** Pinna installs over fastboot. It
  also needs Kaeru, a small addition to the bootloader that the amonet-based
  unlock installs: Kaeru opens fastboot when the system asks for it, and Pinna
  uses that to get back to fastboot after a failed boot. These docs don't
  cover how to unlock a device.
- **A micro-USB cable that carries data.** Some cables only charge. Pinna also
  uses the cable for a serial console and a network connection to your
  computer.

## Backup of the device

Before you install Pinna, back up every partition of the device. Pinna can
restore the backup, and the backup is also where the Wi-Fi firmware comes
from.

To restore it with `scripts/flash-dot.sh restore`, keep the partition images
in one folder:

| File | Partition | Required |
| --- | --- | --- |
| `boot_a.img` | `boot_a` | Yes |
| `boot_b.img` | `boot_b` | Yes |
| `system_a.img` | `system_a` | Yes |
| `userdata.img` | `userdata` | Yes |
| `system_b.img` | `system_b` | No |
| `SHA256SUMS` | Checksums of the files above, in `sha256sum` format | Yes |

To create `SHA256SUMS`, run this in the folder:

```sh
sha256sum *.img > SHA256SUMS
```

Pinna doesn't provide a tool to create the backup itself. Use the tools that
came with your bootloader unlock, and keep a full image of the eMMC as well as
the individual partitions.

## Computer

- An x86_64 Linux computer with [Nix](https://nixos.org/download/) and flakes
  enabled. The development shell provides the build tools and `fastboot`.
- Permission to use the device over USB without `sudo`. On most
  distributions, install the Android udev rules (often packaged as
  `android-udev-rules`) for fastboot, and add yourself to the `dialout` group
  for the serial console.
- `picocom`, or another serial terminal, from your distribution's packages.
- Network access during the build, for the toolchain, the Debian packages, the
  kernel source and the wake word models.

## Your own files

Put these files into `secrets/`. Git ignores the folder, so they never end up
in the repository.

| File | What it's for | How to create it |
| --- | --- | --- |
| `secrets/release.key` | Signs the root filesystem images. Required | `just keygen` |
| `secrets/machine-id` | The machine ID of the image. Optional | `systemd-id128 new > secrets/machine-id` |
| `secrets/authorized_keys` | Your SSH public key. Without it, you can't log in over SSH | Copy your public key, for example `~/.ssh/id_ed25519.pub` |
| `secrets/wifi/wpa_supplicant-wlan0.conf` | Your Wi-Fi network, so the device connects on first boot. Optional: you can also set it up on the device | See [Connect to Wi-Fi](/docs/flash/#connect-to-wi-fi) |
| `secrets/device/pinnad.toml` | Settings for this device, such as its name. Optional | See [Configure Pinna](/docs/configure/) |
| `secrets/device/esphome-key` | The encryption key for the Home Assistant connection. Optional | See [Connect to Home Assistant](/docs/home-assistant/) |

## Wake word models

Pinna uses the openWakeWord models for "Alexa". `just models` downloads them
during the build. The models are licensed under CC BY-NC-SA 4.0, which doesn't
allow commercial use, so they aren't part of the repository.

## Home Assistant

To use Pinna as a voice satellite, you need a Home Assistant instance on the
same network as the device, with:

- The ESPHome integration, which Home Assistant includes.
- An Assist pipeline with speech-to-text and text-to-speech. Pinna sends the
  audio to the pipeline and plays the reply.

## What's next

[Build Pinna](/docs/build/).
