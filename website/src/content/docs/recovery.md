---
title: Recover a device
description: Get back into fastboot, recover from a failed update or a device that doesn't start, and restore your backup.
---

The bootloader of the Echo (2nd gen) and the Echo Dot (2nd gen) opens
fastboot when you hold **Volume Down** while you connect the device, whatever
software is installed. From fastboot, you can install Pinna again or restore
your backup. This page covers each situation, from the most common to the
least. The Echo Dot (5th gen)'s fastboot is locked, so the fastboot steps
don't apply to it. On it, TWRP stays in slot B: hold **Volume Up** while you
power it on, and install Pinna again from there. See
[If it doesn't start](/docs/flash/#if-it-doesnt-start) and
[The way back](/docs/flash/#the-way-back).

## Start fastboot

To open fastboot:

1. Unplug the device.
2. On an Echo (2nd gen), connect the USB connection to your computer first.
3. Hold **Volume Down**, and plug it in: the micro-USB cable on an Echo Dot
   (2nd gen), or the power adapter on an Echo (2nd gen).
4. When the light ring changes, release the button.
5. Check that your computer sees the device:

   ```sh
   fastboot devices
   ```

If the device runs Pinna and you're logged in, you can also run:

```sh
pinna-bootctl reboot-bootloader
```

To leave fastboot and start the installed system again, run
`fastboot reboot`.

### The fastboot guard

Early in every boot, Pinna asks the bootloader to open fastboot on the next
boot. When the boot succeeds and Pinna's health check passes, Pinna cancels
the request. If the health check fails, Pinna restarts the device, and it
opens fastboot. The same happens if the device hangs and you unplug it. This
way, the device doesn't keep booting a broken system.

The health check runs about 10 seconds after Pinna's services start, and fails
if `pinnad` doesn't answer within another 30 seconds or SSH doesn't run.

To see whether a request is pending, run:

```sh
pinna-bootctl status
```

`command: BootonceBootloader` means the next boot opens fastboot.
`command: Empty` means it doesn't. `kaeru: true` confirms that the Kaeru
payload, which handles the request, is present.

Pinna doesn't make the request while it tries a new update, because a failed
attempt already returns to the previous system. See
[Update Pinna](/docs/update/).

## Recover from a failed update

You usually don't need to do anything. Pinna tries a new update up to three
times. If it doesn't pass the health check in that time, the device goes back
to the previous system on its own: its kernel and its root filesystem. A new
version that fails while it starts goes back on its own too.

If the ring stays dark, the device stays in rescue mode, or it doesn't come
back within a few minutes, restart it: select **Restart** on
`http://10.42.0.1`, or unplug the device and plug it in again. Do this up to
three times. Each start of the new version uses one attempt, and after the
last one the bootloader starts the previous version. A kernel that hangs
instead of restarting needs this.

To see what happened, run:

```sh
pinna-bootctl slot-status
```

The output lists both slots with their version and state: `good`, `trial` or
`unbootable`, the attempts left, and which slot is running.

To go back to the other slot yourself, use **Switch…** on the dashboard's
**System > Updates** page. See
[Switch to the other version](/docs/update/#switch-to-the-other-version).

If the dashboard doesn't open, a developer can switch over SSH (see
[Log in to a device](/docs/development/#log-in-to-a-device)):

1. Choose the slot for the next boot. Replace `a` with the slot that you want:

   ```sh
   pinna-bootctl set-active a
   ```

   Pinna checks the slot's signature before it switches. The slot boots on
   trial, with three attempts.

2. Restart the device:

   ```sh
   systemctl reboot
   ```

## Rescue mode

When the root filesystem in the slot that started can't be verified or
mounted, Pinna marks that slot unbootable and restarts into the other slot,
if the other slot is marked good, and so does a new version on trial that
fails in any other way. Otherwise, and when Pinna can't find the storage's
partitions or can't start the system, it doesn't restart. It stays
in rescue mode, so you can find out why. In rescue mode:

- The light ring is green. It gets slowly brighter and dimmer until a
  computer connects over USB, then it stays on.
- Your computer sees a USB device named **Pinna rescue**. The device is at
  `10.42.0.1`, and your computer gets an address between `10.42.0.10` and
  `10.42.0.29`, as in normal operation.
- `http://10.42.0.1` shows why the device is in rescue mode, what usually
  fixes it, and Pinna's log from the start. **Download diagnostics** saves
  every log in one text file: attach it when you
  [report a problem](/docs/troubleshooting/#report-a-problem). **Restart**
  boots the same way again, so it ends in rescue mode again unless something
  changed. On a new version on trial, each restart uses one of its attempts.
- The USB serial console has a root shell: `picocom -b 115200 /dev/ttyACM0`.
  SSH doesn't run in rescue mode.

Until USB is up, the ring flashes white instead, pauses and repeats. If it
keeps flashing, USB didn't come up either. The number of flashes is the
reason, and the page shows the same reason by name:

| Flashes | Reason | What usually fixes it |
| --- | --- | --- |
| 1 | `no-storage`: Pinna didn't find the storage's partitions within 15 seconds | The kernel can't drive the storage. Write a boot image that worked before: `BOOT_IMG=<file> scripts/flash-gen2.sh boot` |
| 2 | `no-root`: the slot that started holds no root filesystem that Pinna can verify and mount, and the other slot isn't marked good | Write a root filesystem and a boot image from the same build. If the log says the signature doesn't verify, the two were signed with different keys |
| 3 | `no-system`: the root filesystem was mounted, but the system on it didn't start | Write the root filesystem again, and a boot image from the same build |

A `crashed` reason means that Pinna's own start-up program failed. Write a
boot image that worked before, and report the problem with the diagnostics.

The same white flashes carry on past rescue mode: 4 is USB, 5 the Wi-Fi
driver, 6 `pinnad`. Those don't mean the device is in rescue mode — Pinna is
running; see [Light ring and buttons](/docs/ring-and-buttons/#while-the-device-starts)
for what each step means.

To write an image, [start fastboot](#start-fastboot) with **Volume Down**.
Writing a boot image keeps your settings. Writing a root filesystem with
`scripts/flash-gen2.sh root` erases the data partition, as described in
[Recover a device that doesn't start](#recover-a-device-that-doesnt-start).

## Recover a device that doesn't start

To start over on a device that still runs Pinna, use
[Factory reset](/docs/settings/#reset-the-device) on the dashboard instead:
it keeps the installed version and needs no fastboot.

If the device doesn't boot Pinna at all, install it again from fastboot.
This erases the data partition: the Wi-Fi networks, every setting made on the
dashboard or in Home Assistant, standalone mode's services and keys, and the
Home Assistant encryption key, unless the image sets them up. After that,
connect the device to Wi-Fi and set it up on the dashboard again. If you use Home Assistant, remove the device
from Home Assistant and add it again, so that Home Assistant gives it a new
key.

1. [Start fastboot](#start-fastboot).
2. Check the device:

   ```sh
   scripts/flash-gen2.sh check
   ```

3. Write a root filesystem that you know works. Keep a copy of one:
   `just release` replaces the files in `build/release/` each time.
   To write another image than `build/pinna.root.img`, set `ROOT_IMG`:

   ```sh
   ROOT_IMG=~/pinna-releases/pinna-0.4.0.root.img \
     scripts/flash-gen2.sh root
   ```

4. Write the boot image:

   ```sh
   scripts/flash-gen2.sh boot
   ```

For the details of each step, see [Install Pinna](/docs/flash/).

## Restore your backup

To go back to the software that ran before Pinna, restore your backup from
fastboot:

1. [Start fastboot](#start-fastboot).
2. Restore the partitions:

   ```sh
   scripts/flash-gen2.sh restore ~/echo-backups/my-dot/partitions
   ```

   Replace the path with the folder that holds your partition images and
   `SHA256SUMS`, as described in
   [What you need](/docs/requirements/#backup-of-the-device).

The script checks the files against `SHA256SUMS` and asks you to confirm.
It then writes `boot_a`, `boot_b`, `system_a`, `system_b` (if the backup
contains it) and `userdata`, sets the bootloader to start slot A, and restarts
the device.

## What's next

- If the device starts but doesn't work as expected, see
  [Troubleshooting](/docs/troubleshooting/).
- To report a problem, see
  [Report a problem](/docs/troubleshooting/#report-a-problem). In rescue mode,
  attach the diagnostics file from `http://10.42.0.1`.
