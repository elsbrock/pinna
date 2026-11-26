---
title: Install on the Echo Dot (5th gen)
description: Install Pinna on an Echo Dot (5th gen) that firebreak unlocked, from TWRP over USB, and connect it to Wi-Fi.
---

This page is for the Echo Dot (5th gen), model C2N6L4. It isn't for the
Echo Dot with clock (5th gen) or the Echo Pop. For the Echo (2nd gen) and the
Echo Dot (2nd gen), see [Install Pinna](/docs/flash/).

The Echo Dot (5th gen)'s bootloader has a fastboot mode, but it is locked:
it can't install Pinna. You install Pinna from TWRP,
which firebreak leaves on the device, with a script that runs on your
computer and writes to the device over USB. Every step says what it writes,
and [The way back](#the-way-back) comes before the first one.

## Before you begin

- **The device is unlocked with firebreak, and you kept its backup.**
  [firebreak's guide for this device](https://github.com/Gamer92000/firebreak/blob/main/docs/cannoli.md)
  unlocks it and leaves a rooted Fire OS in slot A and TWRP in slot B. Run
  the unlock with its `--backup` option, and keep the folder it writes: it
  is the only way back to Fire OS. Pinna doesn't ship firebreak.
- **A USB connection to the device.** The Echo Dot (5th gen) has no USB
  connector. Its USB is the test pads under its foot: solder D+, D− and
  ground to a USB cable, as firebreak's guide describes.
- **A Linux computer** with `bash`, `adb`, `sha256sum`, `stat` and `awk`.
  The script names any tool it can't find. To use adb without `sudo`,
  install your distribution's Android udev rules, as described in
  [What you need](/docs/requirements/#computer).
- **A Pinna release folder built for the Echo Dot (5th gen).** No published
  release includes its files yet. Build them as described in
  [Build for the Echo Dot (5th gen)](/docs/build/#build-for-the-echo-dot-5th-gen):
  the folder is `build/release/`, with `install-echo-dot-5.sh`,
  `pinna-boot-mt8519.itb`, `pinna-<version>.root.img`,
  `pinna-bootctl-armv7` and `SHA256SUMS`.

## What gets written

The script writes three things on the device, and nothing else:

| On the device | What it gets |
| --- | --- |
| `boot_a` | Pinna's boot image (`pinna-boot-mt8519.itb`) |
| `system_a` | Pinna's root image |
| The slot record in `misc` | Slot A becomes the slot that starts, on trial |

This replaces Fire OS in slot A. `boot_b` (TWRP), `system_b`, `userdata` and
every other volume stay as they are. Pinna keeps its settings in `userdata`.

Before it writes, the script checks the boot image, the root image and
`pinna-bootctl-armv7` against `SHA256SUMS`. After writing, it reads each
volume back and compares it again.

## The way back

- **To TWRP:** unplug the power, then hold **Volume Up** while you plug it
  in. This works as long as slot B holds TWRP.
- **To Fire OS:** firebreak's restore puts Fire OS back into slot A from the
  backup its unlock made. It doesn't restore `userdata` or slot B. See
  [firebreak's guide](https://github.com/Gamer92000/firebreak/blob/main/docs/cannoli.md).

Over-the-air updates aren't available on the Echo Dot (5th gen) yet. When
they are, the first update writes slot B and so replaces TWRP. Read
firebreak's guide before you install one.

## Install

1. Start TWRP: unplug the power, then hold **Volume Up** while you plug it
   in, and keep holding it for a few seconds. The device has no screen.
2. Connect the USB cable to your computer. On your computer, check that adb
   sees the device in TWRP:

   ```sh
   adb devices
   ```

   The list shows one device, followed by `recovery`. If it says `device`
   instead, Fire OS started: go back to step 1. If it says
   `no permissions`, install the udev rules (see
   [Before you begin](#before-you-begin)).

3. On your computer, in the release folder, check the device and the files.
   This writes nothing:

   ```sh
   bash install-echo-dot-5.sh check
   ```

   It shows the TWRP version, the device's `device_type_id`
   (`A4ZXE0RM7LQ7A`), its UBI volumes, and each file's size next to the
   space it goes into. Then it makes the same checks as the install, and
   ends with:

   ```text
   ==> ready to install: nothing was written
   ```

   A problem ends the output with a red `error:` line that names it. To stop
   here, unplug the power and plug it in again without holding a button:
   Fire OS starts.

4. Install, on your computer, in the release folder:

   ```sh
   bash install-echo-dot-5.sh install
   ```

   The script lists what it writes and asks you to type `yes`. It then
   writes `boot_a` and `system_a`, sets slot A to start, and restarts the
   device. The root image takes about 20 seconds and shows no progress
   meanwhile; don't disconnect the device.

   If the script stops with an error, the device is still in TWRP and its
   slot record is unchanged. Fix the cause, and run `install` again before
   you restart the device.

## Check the device

Within a minute, your computer sees a new USB device named
**Pinna (Echo Dot 5th gen)**. Open `http://10.42.0.1/` in a browser: the
dashboard appears.

On its first start, Pinna checks that it is healthy and keeps slot A. The
ring shows the boot stages, as described in
[Light ring and buttons](/docs/ring-and-buttons/#while-the-device-starts).

## Connect to Wi-Fi

Over the USB cable, follow
[Over the USB cable](/docs/flash/#over-the-usb-cable): add your network on
**Network**, then select **Keep it** when the dashboard asks, or the device
goes back after 5 minutes. The setup hotspot works too, as described in
[Over the setup hotspot](/docs/flash/#over-the-setup-hotspot); on the Echo
Dot (5th gen)'s Wi-Fi chip it isn't tested yet.

Then [set up the satellite](/docs/dashboard/): the dashboard first offers a
password.

## If it doesn't start

- **Pinna fails its health check, or doesn't start at all:** the bootloader
  counts the starts of slot A, and after a few it starts slot B, which is
  TWRP.
- **Your computer sees a USB device named Pinna rescue:** Pinna couldn't
  start its system. Open `http://10.42.0.1/` to see why.
- **The ring stays blue, and your computer sees a USB device named USB
  download gadget:** the bootloader stopped in its fastboot mode. This was
  seen after a reset without a shutdown and after the power was cut during a
  restart. On your computer, run `fastboot`, from the same Android tools as
  `adb`:

  ```sh
  fastboot continue
  ```

  The device continues to start Pinna.

To install again, start TWRP yourself: unplug the power, then hold
**Volume Up** while you plug it in. From TWRP, run `install` again;
`userdata` and your settings stay.

## Install a newer Pinna

Until over-the-air updates are available on the Echo Dot (5th gen), install
a newer release the same way: start TWRP, then run the newer release
folder's `install`. Your settings in `userdata` stay.
