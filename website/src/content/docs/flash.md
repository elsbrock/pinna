---
title: Install Pinna
description: Install Pinna on an Echo Dot (2nd gen) over fastboot, check that it works, and connect it to Wi-Fi.
---

This guide installs Pinna on an Echo Dot (2nd gen) over fastboot. You install
the root filesystem first, then the boot image. Every step says what it
writes, and [Recover a device](/docs/recovery/) explains how to undo it.

## Before you begin

- Build the images, as described in [Build Pinna](/docs/build/). You need
  `build/pinna-boot.img` and `build/pinna.root.img`.
- Back up the device, as described in
  [What you need](/docs/requirements/#backup-of-the-device). Installing Pinna
  erases the data partition.
- Run the commands in this guide from the repository folder, inside the
  development shell (`nix develop`).

## What gets written

| Partition | After installation |
| --- | --- |
| `boot_a` | Pinna's boot image. The bootloader always starts this partition |
| `boot_b` | A second copy of Pinna's boot image |
| `system_a` | Pinna's root filesystem |
| `system_b` | Its beginning is cleared. Pinna installs updates here |
| `userdata` | Its beginning is cleared. Pinna creates its data partition (`/var`) here on first boot |
| `misc` | Pinna stores its fastboot request and its update state here |

Pinna never writes these partitions:

- The bootloader: the preloader, `lk_a`, `lk_b`, `tee1` and `tee2`.
- `expdb`, which holds Kaeru.
- `persist`, which holds the device's identity, calibration data and MAC
  address.

## Start fastboot

1. Unplug the Echo Dot.
2. Hold **Volume Down**, and plug the micro-USB cable into the device and your
   computer.
3. When the light ring changes, release the button.
4. Check that your computer sees the device:

   ```sh
   fastboot devices
   ```

5. Check the device and the images:

   ```sh
   scripts/flash-dot.sh check
   ```

   The output shows the product name, which must be `BISCUIT`, the
   partition sizes, and the checksums of the two images. This step writes
   nothing.

## Install the root filesystem

1. Write the root filesystem:

   ```sh
   scripts/flash-dot.sh root
   ```

   The script checks the image signature against `keys/release.pub`, asks you
   to confirm, writes `system_a`, and clears the start of `system_b` and
   `userdata`. After this step, the data on `userdata` is gone.

2. If fastboot rejects the image because it's larger than the device accepts
   in one transfer, convert it to a sparse image and write that instead:

   ```sh
   img2simg build/pinna.root.img build/pinna.root.simg
   ROOT_IMG=build/pinna.root.simg scripts/flash-dot.sh root
   ```

   The script can't check the signature of a sparse image. Convert only the
   image that you built and that passed the check in step 1.

## Install the boot image

The device is still in fastboot after the previous step. Write the boot image:

```sh
scripts/flash-dot.sh boot
```

The script asks you to confirm, writes `boot_a` and `boot_b`, and restarts the
device. From now on, the device starts Pinna. Until this step, the previous
firmware doesn't boot properly.

## Check the device

1. Wait about 20 seconds. Your computer sees a new USB device named
   **Pinna (Echo Dot 2nd gen)**, which provides a serial console and a network
   connection.

   If no USB device appears within a minute, Pinna didn't boot. Unplug the
   device, open fastboot, and check that `scripts/flash-dot.sh root` finished
   without errors. Pinna's early boot messages only go to the board's internal
   serial port, not to USB.

2. Log in. Use either connection:

   - Over the network: your computer gets an address from the device
     automatically. Run `ssh root@10.42.0.1` or `ssh root@pinna.local`. This
     needs the SSH key from `secrets/authorized_keys`.
   - Over the serial console: run `picocom -b 115200 /dev/ttyACM0`. You're
     logged in as `root` without a password.

3. Check that Pinna runs:

   ```sh
   pinnactl status
   systemctl --failed
   ```

   `pinnactl status` shows the device's name, its state and its volume.
   `systemctl --failed` lists no units.

4. Check the LED ring:

   ```sh
   pinnactl led-test
   ```

   The ring shows the listening, thinking, speaking and muted animations.

5. Check the speaker: press **Volume up** on the device. You hear a short
   beep, and the ring shows the new volume.

## Connect to Wi-Fi

If you put your Wi-Fi settings into `secrets/wifi/wpa_supplicant-wlan0.conf`
before the build, the device connects on its own. To set up Wi-Fi on the
device instead:

1. Log in to the device over USB, as described in
   [Check the device](#check-the-device).
2. Create or edit `/var/lib/pinna/wifi/wpa_supplicant-wlan0.conf`, for
   example with `vi`. It's a standard wpa_supplicant configuration file:

   ```text
   ctrl_interface=/run/wpa_supplicant
   update_config=1

   network={
       ssid="Your network"
       psk="Your password"
   }
   ```

3. Restart the Wi-Fi client:

   ```sh
   systemctl restart wpa_supplicant@wlan0
   ```

4. Check the connection:

   ```sh
   ip addr show wlan0
   ```

The file on the device takes precedence over the file in the image. Updates
don't change it.

## Check that the installation is healthy

Pinna has two copies of its root filesystem, called slots: `system_a` and
`system_b`. After each boot, Pinna checks itself: `pinnad` must answer and SSH
must run. When the check passes, Pinna marks the slot as good. To see the
result, run:

```sh
pinna-bootctl slot-status
```

The output shows which slot is running and whether it's marked `good`.

If the check fails, the device restarts and opens fastboot. See
[The device opens fastboot on its own](/docs/troubleshooting/#the-device-opens-fastboot-on-its-own).

## What's next

[Connect the device to Home Assistant](/docs/home-assistant/).
