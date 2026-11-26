---
title: Install on the Echo Dot (5th gen)
description: Install Pinna on an Echo Dot (5th gen) that firebreak unlocked, from TWRP over USB, and connect it to Wi-Fi.
---

This page is for the Echo Dot (5th gen), model C2N6L4. It isn't for the
Echo Dot with clock (5th gen) or the Echo Pop. For the Echo (2nd gen) and the
Echo Dot (2nd gen), see [Install Pinna](/docs/flash/).

The Echo Dot (5th gen) has no fastboot mode. You install Pinna from TWRP,
which firebreak leaves on the device, with a script on your computer that
writes over USB. Every step says what it writes, and
[The way back](#the-way-back) explains how to undo it.

## Before you begin

- **The device is unlocked with [firebreak](https://github.com/Gamer92000/firebreak).**
  firebreak leaves a rooted Fire OS in slot A and TWRP in slot B. See
  [Echo devices](/docs/devices/#echo-dot-5th-gen).
- **A USB connection to the device.** The Echo Dot (5th gen) has no USB
  connector: its USB is the test pads under its foot, with a cable soldered
  to them, as firebreak's guide describes.
- **On your computer:** `bash`, `adb`, `sha256sum`, `stat` and `awk`. The
  script names any tool it can't find.
- **A Pinna release built for the Echo Dot (5th gen).** Its folder has
  `install-echo-dot-5.sh`, `pinna-boot-mt8519.itb`, the root image
  `pinna-<version>.root.img`, `pinna-bootctl-armv7` and `SHA256SUMS`. To
  build these yourself, see [Build Pinna](/docs/build/).

## What gets written

The script writes three things, and nothing else:

| On the device | What it gets |
| --- | --- |
| `boot_a` | Pinna's boot image (`pinna-boot-mt8519.itb`) |
| `system_a` | Pinna's root image |
| The slot record in `misc` | Slot A becomes the slot that starts, on trial |

Fire OS in slot A is replaced. TWRP in `boot_b`, `system_b`, `userdata` and
every other part of the device stay as they are. Pinna keeps its settings in
`userdata`, next to what Fire OS left there.

Before writing, the script checks every file against `SHA256SUMS`, and
after writing it reads each part back and compares it again.

## Install

1. Start TWRP: hold Volume Up while you connect the power, and keep holding
   it for a few seconds.
2. Connect the USB cable to your computer. The device has no screen; TWRP is
   up when adb lists it. On your computer, check that adb sees one device:

   ```sh
   adb devices
   ```

3. In the release folder, on your computer, check the device and the files.
   This writes nothing:

   ```sh
   ./install-echo-dot-5.sh check
   ```

   It shows the TWRP version, the device's model ID (`A4ZXE0RM7LQ7A`), its
   storage and how large each file is next to the space it goes into. Stop
   here if it reports a problem.

4. Install, on your computer:

   ```sh
   ./install-echo-dot-5.sh install
   ```

   It lists what it writes and asks you to type `yes`. It then writes
   `boot_a` and `system_a`, sets slot A to start and restarts the device.
   Writing the root image over USB takes a few minutes.

The device starts Pinna. The light ring shows the boot stages, as described
in [Ring and buttons](/docs/ring-and-buttons/). On its first start Pinna
checks that it is healthy and keeps slot A.

## Connect to Wi-Fi

Open the dashboard over USB, at `http://10.42.0.1/` from your computer, and
add your network under **Network**. You can also use the setup hotspot, as
described in [Install Pinna](/docs/flash/#connect-to-wi-fi); on the Echo
Dot (5th gen)'s Wi-Fi chip it isn't tested yet.

Then [set up the satellite](/docs/dashboard/).

## If it doesn't start

If Pinna starts but fails its health check, it restarts and tries slot A
again a few times, and then starts slot B, which is TWRP. If the device
doesn't get as far as Pinna's start, it keeps restarting slot A and doesn't
go back to TWRP by itself.

In either case you can start TWRP yourself: hold Volume Up while you connect
the power. From TWRP, run the install again.

If Pinna starts but something fails, its rescue page is at
`http://10.42.0.1/` over USB. See [Recover a device](/docs/recovery/).

## The way back

- **To Fire OS:** firebreak restores the Fire OS it backed up when it
  unlocked the device. See firebreak's guide.
- **To TWRP:** hold Volume Up while you connect the power, as long as slot B
  still holds TWRP.

Over-the-air updates aren't available on the Echo Dot (5th gen) yet. When
they are, the first update writes slot B and so replaces TWRP: after it both
slots hold Pinna, and the way back to TWRP or Fire OS is firebreak again.
