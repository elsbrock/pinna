---
title: Recover a device
description: Get back into fastboot, recover from a failed update or a device that doesn't start, and restore your backup.
---

You can always get an Echo Dot running Pinna back into fastboot, and from
there install Pinna again or restore your backup. This page covers each
situation, from the most common to the least.

## Start fastboot

Fastboot works the same with any software on the device, so this always
works:

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

Early in every start, Pinna asks the bootloader to open fastboot on the next
start. When the start succeeds and Pinna's health check passes, Pinna cancels
the request. If the device hangs before that and you unplug it, it starts in
fastboot instead of trying the same broken system again.

To see whether a request is pending, run:

```sh
pinna-bootctl status
```

`command: BootonceBootloader` means the next start opens fastboot.
`command: Empty` means it doesn't. `kaeru: true` confirms that the Kaeru
payload, which handles the request, is present.

Pinna doesn't arm the guard while it tries a new update for the first time,
because a failed attempt already returns to the previous system. See
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

To go back to the other slot yourself, run:

```sh
pinna-bootctl set-active a
```

Replace `a` with the slot that you want. The slot starts on trial with three
attempts, and Pinna checks its signature before it switches.

## Recover a device that doesn't start

If the device doesn't start Pinna at all, install it again from fastboot:

1. [Start fastboot](#start-fastboot).
2. Check the device:

   ```sh
   scripts/flash-dot.sh check
   ```

3. Write a root filesystem that you know works:

   ```sh
   scripts/flash-dot.sh root
   ```

4. Try the boot image from memory before you write it:

   ```sh
   scripts/flash-dot.sh try
   ```

5. If the device starts, write the boot image:

   ```sh
   scripts/flash-dot.sh boot
   ```

For the details of each step, see [Install Pinna](/docs/flash/).

If Pinna can't start any root filesystem slot, it restarts after five
seconds. In that case the fastboot guard can't help, because it runs later in
the start. Start fastboot with **Volume Down**.

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
