---
title: What you need
description: The device, computer, files and Home Assistant setup that you need before you build and install Pinna.
---

Collect everything on this page before you [build Pinna](/docs/build/). Only
the backup comes from your own device. Pinna includes the device's Wi-Fi
firmware, unmodified, in `firmware/`.

## Device

- **An Echo (2nd gen) or an Echo Dot (2nd gen).** In fastboot, the Echo Dot
  reports the product name `BISCUIT` and the Echo reports `RADAR`. Pinna
  doesn't support other Echo devices yet.
- **An unlocked bootloader with Kaeru.** Pinna installs over fastboot. It
  also needs Kaeru, a small addition to the bootloader that the amonet-based
  unlock installs: Kaeru opens fastboot when the system asks for it, and Pinna
  uses that to get back to fastboot after a failed boot. These docs don't
  cover how to unlock a device.
- **A USB connection to the device.** On an Echo Dot (2nd gen), a micro-USB
  cable that carries data; some cables only charge. On an Echo (2nd gen), the
  pins in the base: a pogo-pin adapter, or a cable soldered to the pads.
  Pinna also uses the connection for a serial console and a network link to
  your computer.

## Backup of the device

Before you install Pinna, back up the device. Pinna can restore the backup.

Run `scripts/backup-dot.sh` while the previous firmware still runs on the
device and `adb` can reach it. It needs about 8 GB of free space:

```sh
scripts/backup-dot.sh ~/echo-backups/echodot2-$(date +%F)
```

The script copies the whole eMMC and its boot areas, and compares every
partition except `userdata` with the device. It also writes the partition
images that `scripts/flash-gen2.sh restore` takes, into `partitions/`, with
their checksums. To compare a backup with the device again later, run
`scripts/verify-backup.sh` with the same folder.

To restore a backup with `scripts/flash-gen2.sh restore`, the partition images
must be in one folder:

| File | Partition | Required |
| --- | --- | --- |
| `boot_a.img` | `boot_a` | Yes |
| `boot_b.img` | `boot_b` | Yes |
| `system_a.img` | `system_a` | Yes |
| `userdata.img` | `userdata` | Yes |
| `system_b.img` | `system_b` | No |
| `SHA256SUMS` | Checksums of the files above, in `sha256sum` format | Yes |

`scripts/backup-dot.sh` creates this folder. If you made the backup with other
tools, create `SHA256SUMS` by running this in the folder:

```sh
sha256sum *.img > SHA256SUMS
```

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

Pinna ships the openWakeWord models for **Alexa**, **Hey Jarvis**, **Hey
Mycroft** and **Hey Rhasspy** (v0.5.1), and **Okay Nabu** from
[`rhasspy/pyopen-wakeword`](https://github.com/rhasspy/pyopen-wakeword), pinned
to a commit. `just models` downloads them during the build; they aren't part of
the repository. The openWakeWord models are licensed under CC BY-NC-SA 4.0,
which doesn't allow commercial use. Okay Nabu comes from a repository under
Apache-2.0; the model itself carries no licence of its own.

## Home Assistant

To use Pinna as a voice satellite, you need a Home Assistant instance on the
same network as the device, with:

- The ESPHome integration, which Home Assistant includes.
- An Assist pipeline with speech-to-text and text-to-speech. Pinna sends the
  audio to the pipeline and plays the reply.

## What's next

[Build Pinna](/docs/build/).
