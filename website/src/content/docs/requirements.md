---
title: What you need
description: The device, the computer and the backup that you need before you install Pinna, and what answers your requests.
---

Collect everything on this page before you [install Pinna](/docs/flash/).
Only the backup comes from your own device. Pinna includes the device's Wi-Fi
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

Back up the device before you install Pinna if you want to go back to the
previous firmware later: installing Pinna replaces it, and only a backup
brings it back. Pinna can restore the backup.

The backup script is in Pinna's repository. Get it on your computer with
`git clone https://github.com/elsbrock/pinna.git`, and run the commands below
in that folder. Run `scripts/backup-gen2.sh` while the previous firmware still
runs on the device and `adb` can reach it. It needs about 8 GB of free space:

```sh
scripts/backup-gen2.sh ~/echo-backups/echodot2-$(date +%F)
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

`scripts/backup-gen2.sh` creates this folder. If you made the backup with other
tools, create `SHA256SUMS` by running this in the folder:

```sh
sha256sum *.img > SHA256SUMS
```

## Computer

To install from the browser: a computer with Chrome, Edge, Brave or Opera.
Firefox and Safari can't talk to USB devices. On Linux the browser needs a
udev rule, and on Windows the WinUSB driver; the [installer](/install/) says
how.

To install from a terminal, and to make the backup: a Linux computer with
`fastboot`, `adb` and `sha256sum` (on Debian/Ubuntu, `android-tools` has the
first two), and:

- Permission to use the device over USB without `sudo`. On most
  distributions, install the Android udev rules (often packaged as
  `android-udev-rules`) for fastboot, and add yourself to the `dialout` group
  for the serial console.
- `picocom`, or another serial terminal, from your distribution's packages.

To build Pinna yourself, you need an x86_64 Linux computer; see
[Build Pinna](/docs/build/).

## Something that answers

Pinna needs something to answer your requests. You choose on the dashboard
after you install it, and you can switch later:

- **Home Assistant**, on the same network as the device, with an Assist
  pipeline that has speech-to-text and text-to-speech. The ESPHome
  integration, which Home Assistant includes, adds the device. See
  [Connect to Home Assistant](/docs/home-assistant/).
- **Standalone mode**, without Home Assistant: a speech-to-text service, a
  brain (a language model or an AI agent) and a text-to-speech service. They
  can be hosted services, with a key from your account, or servers on your own
  network. See [Set up standalone mode](/docs/standalone/).

## What's next

[Install Pinna](/docs/flash/).
