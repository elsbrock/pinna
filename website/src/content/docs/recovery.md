---
title: Recovery
description: The fastboot guard, getting back into fastboot, and restoring the previous firmware.
---

Two mechanisms keep a bad flash from bricking the device: LK's fastboot mode,
which is always reachable by hand, and the fastboot guard, which is armed
automatically on every dev boot.

## Enter fastboot by hand

Unplug the Dot, hold **Volume Down**, plug micro-USB in, release when the ring
changes:

```sh
fastboot devices
scripts/flash-dot.sh check
```

Kaeru's fastboot understands `fastboot reboot`, so a plain reboot returns to
whatever is currently in `boot_a`.

## The fastboot guard

Development images ship `/etc/pinna/fastboot-guard`. When present, every boot
arms a one-shot "enter fastboot" request in the `misc` partition *early* —
before networking and voice come up — and clears it once sshd and `pinnad` are
healthy (`pinna-boot-ok.service`).

The consequence: a boot that hangs, followed by a power cycle, lands in
fastboot instead of looping. A healthy boot clears the request, so nothing
changes for normal operation. Production images omit the guard file.

Check the state from a running system:

```sh
pinna-bootctl status
```

## Restore the previous firmware

With a backup of the original partitions (from before the first flash):

```sh
scripts/flash-dot.sh restore ~/echo-backups/<device>/partitions
```

That rewrites `boot_a`, `boot_b`, `system_a` and `userdata` with the backed-up
images and reboots into the previous firmware. The `verify-backup.sh` script
checks a backup tree against its checksums before you rely on it.

## What is never written

Pinna touches only `boot_a`, `boot_b`, `system_a` and `userdata` (plus the
command field of `misc` at runtime). The preloader, LK, TEE, `expdb` and
`persist` are never written, so device identity, calibration and the
Kaeru/amonet unlock survive any flash of this project.
