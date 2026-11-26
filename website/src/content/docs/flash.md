---
title: Install Pinna
description: Install Pinna on your Echo, from the browser or from a terminal, and connect it to Wi-Fi.
---

How you install Pinna depends on the device:

- **Echo (2nd gen) and Echo Dot (2nd gen):** from the browser or from a terminal
  over fastboot. See [Install from the browser](#install-from-the-browser) and
  [Install from a terminal](#install-from-a-terminal).
- **Echo Dot (5th gen):** from TWRP over USB, with a script. See
  [Install on the Echo Dot (5th gen)](#install-on-the-echo-dot-5th-gen).

For the Echo (2nd gen) and the Echo Dot (2nd gen), you can install Pinna in
two ways. Both write the same partitions:

- **From the browser**, with the [installer](/install/). It downloads the
  newest release, checks its signature and installs it over USB, from
  Chrome, Edge, Brave or Opera.
- **From a terminal**, with `scripts/flash-gen2.sh` and fastboot. Use this
  when the browser can't use the device.

Every step says what it writes, and [Recover a device](/docs/recovery/)
explains how to undo it.

## Before you begin

This section is for the Echo (2nd gen) and the Echo Dot (2nd gen).

- The device's bootloader is unlocked, with Kaeru. See
  [What you need](/docs/requirements/#device).
- If you want to go back to the firmware the device runs now, back it up
  first, as described in
  [What you need](/docs/requirements/#backup-of-the-device). Installing Pinna
  replaces that firmware and erases the data partition.

## What gets written

This section is for the Echo (2nd gen) and the Echo Dot (2nd gen).

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

## Install from the browser

1. Open the [installer](/install/) in Chrome, Edge, Brave or Opera, on a
   computer.
2. Follow its steps: choose your device, get Pinna, start the device in
   fastboot, connect, and install. The installer checks the device and asks
   you to confirm before it writes anything.
3. When it says **Pinna is installed**,
   [connect the device to Wi-Fi](#connect-to-wi-fi).

If the installer can't use the device, it says why and how to fix it. You can
also install from a terminal instead.

## Install from a terminal

You install the root filesystem first, then the boot image.

- Get the images: from a [release](https://github.com/elsbrock/pinna/releases)
  (download all its files into one folder) or by building them, as described
  in [Build Pinna](/docs/build/).
- Run the commands on your computer, from the repository's root folder — in
  `nix develop`, if you built the images with it. The examples say
  `scripts/flash-gen2.sh`; in a release folder, run `./flash-gen2.sh` next to
  the images instead.

### Start fastboot

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

### Install the root filesystem

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

### Install the boot image

The device is still in fastboot after the previous step. Write the boot image:

```sh
scripts/flash-gen2.sh boot
```

The script asks you to confirm, writes `boot_a` and `boot_b`, and restarts the
device. From now on, the device starts Pinna. Until this step, the previous
firmware doesn't boot properly.

### Check the device

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

## Install on the Echo Dot (5th gen)

This section is for the Echo Dot (5th gen), model C2N6L4. It isn't for the
Echo Dot with clock (5th gen) or the Echo Pop.

You install Pinna on the Echo Dot (5th gen) from TWRP,
which firebreak leaves on the device, with a script that runs on your
computer and writes to the device over USB. Every step says what it writes,
and [The way back](#the-way-back) comes before the first one.

### Before you begin on the Echo Dot (5th gen)

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
- **The release files, in one folder:** `install-echo-dot-5.sh`,
  `pinna-boot-mt8519.itb`, `pinna-<version>.root.img`, `pinna-bootctl-armv7`
  and `SHA256SUMS`. Download them from a
  [release](https://github.com/elsbrock/pinna/releases), or build them as
  described in
  [Build for the Echo Dot (5th gen)](/docs/build/#build-for-the-echo-dot-5th-gen),
  which puts them in `build/release/`.

### What gets written on the Echo Dot (5th gen)

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

### The way back

- **To TWRP:** unplug the power, then hold **Volume Up** while you plug it
  in. This works as long as slot B holds TWRP.
- **To Fire OS:** firebreak's restore puts Fire OS back into slot A from the
  backup its unlock made. It doesn't restore `userdata` or slot B. See
  [firebreak's guide](https://github.com/Gamer92000/firebreak/blob/main/docs/cannoli.md).

### Install from TWRP

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
   [Before you begin on the Echo Dot (5th gen)](#before-you-begin-on-the-echo-dot-5th-gen)).

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

### Check the Echo Dot (5th gen)

Within a minute, your computer sees a new USB device named
**Pinna (Echo Dot 5th gen)**. Open `http://10.42.0.1/` in a browser: the
dashboard appears.

On its first start, Pinna checks that it is healthy and keeps slot A. The
ring shows the boot stages, as described in
[Light ring and buttons](/docs/ring-and-buttons/#while-the-device-starts).

### If it doesn't start

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

### Install a newer Pinna

Until over-the-air updates are available on the Echo Dot (5th gen), install
a newer release the same way: start TWRP, then run the newer release
folder's `install`. Your settings in `userdata` stay.

## Connect to Wi-Fi

Set up Wi-Fi on the device's dashboard. You reach it in one of two ways:

- **Over its setup hotspot**, from a phone or computer. While the device has
  no saved Wi-Fi network, it runs its own network, **Pinna setup** followed by
  its hostname, for example `Pinna setup pinna-2a914e`. This needs no cable,
  which helps on the Echo (2nd gen), whose USB is in its base.
- **Over the USB cable**, at `http://10.42.0.1/`.

The setup hotspot is in the releases after 0.4.0-beta.3. It is tested in the
virtual machine, not yet on an Echo. If your phone doesn't find it within two
minutes of the ring fading out, see
[The setup hotspot doesn't appear](/docs/troubleshooting/#the-setup-hotspot-doesnt-appear),
or use the USB cable.

The hotspot's password is the same on every device and published here. While a
device waits for a network, anyone in range can join its hotspot, use its
dashboard, and see what you type there, the password of your Wi-Fi included. A
password that only you can learn is planned.

### Over the setup hotspot

1. On your phone or computer, join the Wi-Fi network whose name starts with
   **Pinna setup**. The password is `pinnasetup`.
2. Most phones then open the dashboard by themselves. If yours doesn't, or
   says the network has no internet, stay connected and open
   `http://10.42.1.1/` in a browser.
3. Open **Network**. Under **Join a network**, select **Scan**. The list can
   be the one from when the hotspot started; if your network is missing,
   select **Join a hidden network** and type its name.
4. Select your network and enter its password.
5. Before you select **Join**, note the address the page gives, such as
   `http://pinna-2a914e.local/`: it is the hotspot's name without
   `Pinna setup`. Note the device's MAC address too.
6. Select **Join**. The page shows **Turning the setup hotspot off to try**
   followed by your network's name, and then loses the device.
7. Wait about two minutes:
   - If **Pinna setup** is back in your phone's Wi-Fi list, the join failed.
     Join the hotspot again and open **Network**, which says why.
   - Otherwise the device is on your network. Within 5 minutes, connect your
     phone to your network and open the address you noted. If it doesn't open,
     which is common on Android, find the MAC address in your router's list of
     devices and open the IP address listed next to it. Opening the dashboard
     at either address keeps the network. If nobody opens it in time, the
     device runs its hotspot again.

### Over the USB cable

1. Connect the device to your computer and open `http://10.42.0.1/` in a
   browser.
2. Open **Network**. Under **Join a network**, select **Scan**.
3. Select your network and enter its password. For a network that doesn't
   show its name, select **Join a hidden network** and type the name.
4. Select **Join**. The device tries the network for up to a minute. If the
   password is wrong, or the network gives it no address, the page says so.
5. When it is on the network, the page asks **Keep** followed by the
   network's name, with the device's new address. Select **Keep it**. Until you
   do, the device goes back after 5 minutes, or when it restarts.

The device joins open networks and WPA2 networks, WPA2/WPA3 networks
included. The Wi-Fi chip of the Echo (2nd gen) and the Echo Dot (2nd gen)
can't join networks that allow only WPA3, enterprise (802.1X) networks or WEP
networks. See
[Change the Wi-Fi network](/docs/settings/#change-the-wi-fi-network).

If you build Pinna yourself, you can give every device that you install the
same network: put a standard wpa_supplicant configuration file into
`secrets/wifi/wpa_supplicant-wlan0.conf` before the build. The device copies
it to `/var/lib/pinna/wifi/` on first boot and connects on its own. The
dashboard saves the networks you join into the same file, and updates keep it.

## Check that the installation is healthy

After each boot, Pinna checks itself: `pinnad` must answer and SSH must run. When the check passes, Pinna marks the slot as good. The
dashboard's **System > Updates** page shows whether the running slot is **Good**. To
see the result from a terminal, run this on the satellite:

```sh
pinna-bootctl slot-status
```

The output shows which slot is running and whether it's marked `good`.

On the Echo (2nd gen) and the Echo Dot (2nd gen), if the check fails, the
device restarts and opens fastboot. See
[The device opens fastboot on its own](/docs/troubleshooting/#the-device-opens-fastboot-on-its-own).
On the Echo Dot (5th gen), see
[If it doesn't start](#if-it-doesnt-start).

## What's next

[Set up the satellite](/docs/dashboard/) on its dashboard. On the Echo Dot
(5th gen), the dashboard first offers a password.
