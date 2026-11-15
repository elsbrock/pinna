---
title: Recover a device
description: Get back into fastboot, recover from a failed update or a device that doesn't start, and restore your backup.
---

The bootloader opens fastboot when you hold **Volume Down** while you connect
the device, whatever software is installed. From fastboot, you can install
Pinna again or restore your backup. This page covers each situation, from the
most common to the least.

## Start fastboot

To open fastboot:

1. Unplug the Echo Dot.
2. Hold **Volume Down**, and plug in the micro-USB cable.
3. When the light ring changes, release the button.
4. Check that your computer sees the device:

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
to the previous system on its own.

To see what happened, run:

```sh
pinna-bootctl slot-status
```

The output lists both root filesystem slots with their version and state:
`good`, `trial` or `unbootable`, and which slot is running.

To go back to the other slot yourself:

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

## Recover a device that doesn't start

If the device doesn't boot Pinna at all, install it again from fastboot.
This erases the data partition: your Wi-Fi settings, the settings from Home
Assistant and the encryption key. After that, set up Wi-Fi again, and if the
connection to Home Assistant was encrypted, give the device its key again.

1. [Start fastboot](#start-fastboot).
2. Check the device:

   ```sh
   scripts/flash-dot.sh check
   ```

3. Write a root filesystem that you know works. Keep a copy of one:
   `just release` replaces the files in `build/release/` each time.
   To write another image than `build/pinna.root.img`, set `ROOT_IMG`:

   ```sh
   ROOT_IMG=~/pinna-releases/pinna-2026.11.15.1432-d032e69.root.img \
     scripts/flash-dot.sh root
   ```

4. Write the boot image:

   ```sh
   scripts/flash-dot.sh boot
   ```

For the details of each step, see [Install Pinna](/docs/flash/).

If Pinna can't boot either root filesystem slot, it restarts after five
seconds. In that case, Pinna can't ask for fastboot, because that happens
later in the boot. Open fastboot with **Volume Down**. Pinna's messages from
this early stage only go to the board's internal serial port, at 921600 baud,
not to USB.

## Restore your backup

To go back to the software that ran before Pinna, restore your backup from
fastboot:

1. [Start fastboot](#start-fastboot).
2. Restore the partitions:

   ```sh
   scripts/flash-dot.sh restore ~/echo-backups/my-dot/partitions
   ```

   Replace the path with the folder that holds your partition images and
   `SHA256SUMS`, as described in
   [What you need](/docs/requirements/#backup-of-the-device).

The script checks the files against `SHA256SUMS` and asks you to confirm.
It then writes `boot_a`, `boot_b`, `system_a`, `system_b` (if the backup
contains it) and `userdata`, and restarts the device.

## What's next

- If the device starts but doesn't work as expected, see
  [Troubleshooting](/docs/troubleshooting/).
- To report a problem, [open an issue](https://github.com/elsbrock/pinna/issues)
  with the output of `pinna-bootctl slot-status` and `journalctl -b`.
