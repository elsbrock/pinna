---
title: Install Pinna
description: Install Pinna on an Echo (2nd gen) or an Echo Dot (2nd gen) over fastboot, check that it works, and connect it to Wi-Fi.
---

This guide installs Pinna on an Echo (2nd gen) or an Echo Dot (2nd gen) over
fastboot. You install the root filesystem first, then the boot image. Every
step says what it writes, and [Recover a device](/docs/recovery/) explains how
to undo it.

## Before you begin

- Get the images: from a [release](https://github.com/elsbrock/pinna/releases)
  (download all its files into one folder) or by building them, as described
  in [Build Pinna](/docs/build/). No release is published yet, so for now you
  build them.
- Back up the device, as described in
  [What you need](/docs/requirements/#backup-of-the-device). Installing Pinna
  erases the data partition.
- Run the commands on your computer, from the repository's root folder — in
  `nix develop`, if you built the images with it. The examples say
  `scripts/flash-gen2.sh`; in a release folder, run `./flash-gen2.sh` next to
  the images instead.

## What gets written

Pinna keeps two copies of its system, called slots: slot A (`boot_a` and
`system_a`) and slot B (`boot_b` and `system_b`). The bootloader's slot
record in `misc` says which one starts.

| Partition | After installation |
| --- | --- |
| `boot_a` | Pinna's boot image (the kernel), slot A |
| `boot_b` | The same boot image, for slot B. The first update goes into slot B |
| `system_a` | Pinna's root filesystem, slot A |
| `system_b` | Its beginning is cleared |
| `userdata` | Its beginning is cleared. Pinna creates its data partition (`/var`) here on first boot |
| `misc` | The bootloader's slot record, set to start slot A. Pinna also stores its fastboot request here |

Pinna never writes these partitions:

- The bootloader: the preloader, `lk_a`, `lk_b`, `tee1` and `tee2`.
- `expdb`, which holds Kaeru.
- `persist`, which holds the device's identity, calibration data and MAC
  address.

## Start fastboot

On an Echo Dot (2nd gen), the micro-USB cable also powers the device and
connects it to your computer. On an Echo (2nd gen), the USB connection is on
the pins in its base (a pogo-pin adapter, or a cable soldered to the pads),
and the device uses its own power adapter.

1. Unplug the device.
2. On an Echo (2nd gen), connect the USB connection to your computer first.
3. Hold **Volume Down**.
4. Plug in the power: on the Echo Dot, the micro-USB cable, with the other end
   on your computer; on the Echo, its power adapter.
5. When the light ring changes, release the button.
6. Check that your computer sees the device:

   ```sh
   fastboot devices
   ```

7. Check the device and the images:

   ```sh
   scripts/flash-gen2.sh check
   ```

   The output shows the product name — `BISCUIT` on an Echo Dot (2nd gen),
   `RADAR` on an Echo (2nd gen) — the partition sizes, and the checksums of
   the two images. This step writes nothing.

## Install the root filesystem

1. Write the root filesystem:

   ```sh
   scripts/flash-gen2.sh root
   ```

   The script checks the files against `SHA256SUMS` when they come from a
   release, and the image signature when you built them yourself. It asks you
   to confirm, writes `system_a`, and clears the start of `system_b` and
   `userdata`. Then it sets the bootloader to start slot A. After this step,
   the data on `userdata` is gone.

2. If fastboot rejects the image because it's larger than the device accepts
   in one transfer, convert it to a sparse image and write that instead:

   ```sh
   img2simg build/pinna.root.img build/pinna.root.simg
   ROOT_IMG=build/pinna.root.simg scripts/flash-gen2.sh root
   ```

   The script can't check the signature of a sparse image. Convert only the
   image that you built and that passed the check in step 1.

## Install the boot image

The device is still in fastboot after the previous step. Write the boot image:

```sh
scripts/flash-gen2.sh boot
```

The script asks you to confirm, writes `boot_a` and `boot_b`, and restarts the
device. From now on, the device starts Pinna. Until this step, the previous
firmware doesn't boot properly.

## Check the device

1. Wait about 20 seconds. Your computer sees a new USB device named
   **Pinna (Echo Dot 2nd gen)** — **Pinna (Echo 2nd gen)** on an Echo — which
   provides a serial console and a network connection.

   If the device is named **Pinna rescue** instead, Pinna found nothing it
   can boot. Open `http://10.42.0.1` to see why. See
   [Rescue mode](/docs/recovery/#rescue-mode). **Restart** on that page
   boots the image in `boot_a` again, not the one you tried from memory. To
   try again, open fastboot and run `scripts/flash-gen2.sh try`.

   If no USB device appears within a minute, Pinna didn't boot. Unplug the
   device, open fastboot, and check that `scripts/flash-gen2.sh root` finished
   without errors.

2. Log in. Use either connection:

   - Over the network: your computer gets an address from the device
     automatically. Run `ssh root@10.42.0.1`. This needs your SSH key on the
     device: `secrets/authorized_keys` is baked in when you built the image; in
     a release image, add yours over the serial console first.
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

5. Check the speaker: press **Volume Up** on the device. You hear a short
   beep, and the ring shows the new volume.

## Connect to Wi-Fi

Set up Wi-Fi on the device's dashboard, over the USB cable:

1. Connect the device to your computer and open `http://10.42.0.1/` in a
   browser.
2. Open **Network**. Under **Join a network**, select **Scan**.
3. Select your network and enter its password. For a network that doesn't
   show its name, select **Join a hidden network** and type the name.
4. Select **Join**. The device tries the network for up to a minute. If the
   password is wrong, or the network gives it no address, the page says so.
5. When it is on the network, the page asks **Keep** followed by the
   network's name, with the device's new address. Select **Keep**. Until you
   do, the device goes back after 5 minutes, or when it restarts.

The device joins open networks and WPA2 networks, WPA2/WPA3 networks
included. Its Wi-Fi chip can't join networks that allow only WPA3, enterprise
(802.1X) networks or WEP networks. See
[Change the Wi-Fi network](/docs/dashboard/#change-the-wi-fi-network).

If you build Pinna yourself, you can give every device that you install the
same network: put a standard wpa_supplicant configuration file into
`secrets/wifi/wpa_supplicant-wlan0.conf` before the build. The device copies
it to `/var/lib/pinna/wifi/` on first boot and connects on its own. The
dashboard saves the networks you join into the same file, and updates keep it.

## Check that the installation is healthy

Pinna has two copies of its system, called slots: slot A (`boot_a` and
`system_a`) and slot B (`boot_b` and `system_b`). After each boot, Pinna checks itself: `pinnad` must answer and SSH
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
