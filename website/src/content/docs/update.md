---
title: Update Pinna
description: Install a new version of Pinna on a running device, and how the device falls back if the new version doesn't work.
---

Pinna keeps two copies of its system, called slots. Slot A is the boot image
in `boot_a` and the root filesystem in `system_a`; slot B is `boot_b` and
`system_b`. The boot image holds the kernel. An update goes into the slot that
isn't running. The device then tries the new version, and if the new version
doesn't work, it goes back to the old one on its own, kernel included.

## How an update works

1. `pinnactl update` downloads the new boot image and root filesystem image
   and writes them to the slot that isn't running. It checks each download
   against its checksum in the update manifest. After writing, it reads the
   boot image back, and checks the root filesystem's signature and its hash
   tree, which lets the device detect any changed block.
2. On the next boot, the bootloader starts the new slot **on trial**, with
   three attempts. The bootloader counts them itself, so a kernel that doesn't
   start uses up attempts too.
3. When the new system is up, Pinna checks itself: `pinnad` must answer and
   SSH must run within five minutes. If the check passes, the slot is marked
   **good** and stays in use.
4. If the check fails or doesn't finish in time, the device restarts. Each
   boot on trial uses one attempt. When no attempts are left, the bootloader
   starts the previous slot, its kernel and its root filesystem, and the new
   slot counts as **unbootable**. A version that fails before its system
   runs, for example a root filesystem that Pinna can't verify or mount,
   doesn't wait for its attempts: the device marks its slot unbootable and
   restarts into the other one straight away.

Pinna only installs images that are signed with the key whose public part is
built into the device: the project key `keys/release.pub` in a release,
`secrets/release.pub` in a build of your own. Build your updates with the
same `secrets/release.key` that you used for the first installation.

## Update to a new release

No release is published yet. Until one is, build your updates as described
in [Install an update](#install-an-update).

A release's `manifest.json` installs the newest published release. On the
device, with Wi-Fi and internet access:

```sh
pinnactl update https://github.com/elsbrock/pinna/releases/latest/download/manifest.json --reboot
```

It writes the new boot image and root filesystem into the other slot, checks
them and restarts into it, as described above.

## Before you begin

- The device runs Pinna, and its current slot is marked `good`. Pinna refuses
  to install an update while the running slot is still on trial. To check, run
  `pinna-bootctl slot-status` on the device.
- The device runs a Pinna built on or after 17 November 2026. An older one
  keeps its own slot record, which the new version doesn't read: move it over
  once, as in [Move a device from an older Pinna](#move-a-device-from-an-older-pinna).
- The device can reach your computer over the network, either over Wi-Fi or
  over the USB cable.
- Your copy of the repository is up to date. `keys/release.pub` is the
  project key and doesn't change; your own signing key lives in `secrets/`.

## Install an update

1. On your computer, build the update files:

   ```sh
   just release
   ```

   The files are in `build/release/`: the root image, the boot image and
   `manifest.json`. `just release` replaces them each time, so copy a release
   that works to a safe place.

2. Serve the folder over HTTP:

   ```sh
   python3 -m http.server 8000 --directory build/release
   ```

3. On the device, install the update and restart:

   ```sh
   pinnactl update http://192.168.1.20:8000/manifest.json --reboot
   ```

   Replace `192.168.1.20` with your computer's address. Over the USB cable,
   use the address that your computer got from the device, in the range
   `10.42.0.10` to `10.42.0.29`.

   `pinnactl` prints each step: the slot it installs into, the boot image
   (`kernel from … written to …`, or why the kernel stays), the root
   filesystem download and its checksum, the signature and the hash tree,
   and which slot the device starts next. Without `--reboot`, the update
   waits for the next restart.

4. After the device restarts, check the result:

   ```sh
   pinna-bootctl slot-status
   ```

   Within a few minutes, the new slot is the running slot and marked `good`:

   ```text
   state: valid
   active: b
   a: good attempts=0 (priority 14, tries 0), version 2026.11.15.1432-d032e69
   b: good attempts=0 (priority 15, tries 0), version 2026.11.17.0910-1a2b3c4
   booted: b (version 2026.11.17.0910-1a2b3c4)
   kernel: from boot_b
   bootloader: loads the kernel of the slot it starts
   ```

   `active` is the slot that starts next. `attempts` counts the boots a slot
   on trial has left; the bootloader stores one try more. `kernel` shows the
   boot partition the running kernel came from.

To give the new version more or fewer attempts than three, add
`--tries <number>` to `pinnactl update`.

## Install an update from the dashboard

The [dashboard](/docs/dashboard/) runs the same update as `pinnactl update`.

1. Check [Before you begin](#before-you-begin), then build and serve the update
   files, as in steps 1 and 2 above.
2. In the dashboard, open **System**. If it says **This version is on trial**,
   wait until the running slot is marked **Good**, within 5 minutes of the last
   restart.
3. Check the **Release manifest** field. It is filled in with port 8000 on the
   computer or phone that you opened the dashboard on, or with the last address
   you used. If the files are served from another computer, change it to that
   computer's address. Then select **Install**.

   The page shows each step. The device keeps running the update if you close
   the page; open **System** again to see where it is. If the update fails,
   the page says **The update was not installed** and gives the reason, and
   **Output on the satellite** shows every line. The device keeps running the
   current version.
4. When the update is installed, select **Restart now**. The page reconnects
   when the device is back, and shows the new version on trial.

   To start it later instead, select **Start it later**. The update stays
   in its slot as **Not started yet**, without **starts next**. To start it,
   see [Switch to the other version](#switch-to-the-other-version).
5. Wait until the new slot is marked **Good**, within 5 minutes of the restart.
   You can install the next update after that.

If the new version doesn't start, the device goes back to the previous one, and
**System** shows the new slot as **Can't boot**. See
[Recover a device](/docs/recovery/) if the device doesn't come back at all.

## Switch to the other version

The slot that isn't running holds the previous version, or an update that you
kept back (**Not started yet**). To run it:

1. In the dashboard, open **System**. If it says **This version is on trial**,
   wait until the running slot is marked **Good**, within 5 minutes of the
   last restart.
2. Under **Software**, select **Switch…** next to **Switch to slot A** or
   **Switch to slot B**. There is no **Switch…** when the other slot is empty
   or marked **Can't boot**.
3. Select **Switch and restart**. The page reconnects when the device is back,
   and shows that version on trial.

The other version starts on trial, like an update. If it doesn't start, the
device goes back to the version that ran before.

Versions built before the switch was added don't have **Switch…**. If you
switch to one of them, install an update to leave it.

## Update the boot image

An update writes the boot image, and with it the kernel, into the slot it
installs, so you don't need fastboot for it. The exception is an older Kaeru,
the bootloader addition that the unlock installs (see
[What you need](/docs/requirements/)): Kaeru builds from before 22 October 2026
load the kernel from `boot_a` even when they start slot B.

Pinna finds out which kind it has. The first update after an installation
goes into slot B. When slot B starts and passes its health check, Pinna knows
whether the kernel came from `boot_b`:

- If it did, later updates into slot A write `boot_a` too.
- If it didn't, updates leave the kernel alone and say so: `the kernel stays:
  this bootloader loads boot_a whichever slot it starts`.

Until Pinna knows, an update into slot A keeps the kernel in `boot_a`, because
a new kernel there might have no other kernel to fall back to. The
`bootloader:` line of `pinna-bootctl slot-status` shows what Pinna knows.

With an older Kaeru, install new boot images from fastboot, on your computer:

```sh
scripts/flash-gen2.sh boot
```

This writes the boot image to `boot_a` and `boot_b` and doesn't change which
slot starts. See [Install Pinna](/docs/flash/#install-the-boot-image).

## Move a device from an older Pinna

A Pinna built before 17 November 2026 keeps the slot state in its own record,
and doesn't write the boot image in updates. Move it over once. This keeps
your settings, but the new version has no previous version to fall back to
until its next update.

1. On the device, install the update without restarting:

   ```sh
   pinnactl update http://192.168.1.20:8000/manifest.json
   ```

   Note the slot in its first line, `installing version … into slot b`.

2. On the device, open fastboot:

   ```sh
   pinna-bootctl reboot-bootloader
   ```

3. On your computer, tell the bootloader to start that slot. Replace `b` with
   the slot from step 1:

   ```sh
   fastboot set_active b
   ```

4. On your computer, write the new boot image to both boot partitions. The
   device restarts into the new version:

   ```sh
   scripts/flash-gen2.sh boot
   ```

If the new version doesn't start, open fastboot (hold **Volume Down** while
you plug the device in) and run `fastboot set_active a` with the other slot.

## Update manifest

`pinnactl update` reads a JSON manifest from an `http://`, `https://` or
`file://` URL, or from a local path:

```json
{
  "version": "2026.11.15.1432-d032e69",
  "root": {
    "url": "pinna-2026.11.15.1432-d032e69.root.img",
    "size": 157286400,
    "sha256": "…"
  },
  "boot": {
    "url": "pinna-boot.img",
    "size": 16777216,
    "sha256": "…"
  }
}
```

| Field | Meaning |
| --- | --- |
| `version` | The image version. It must match the version inside the image |
| `root.url` | Where to download the root image. A relative URL is relative to the manifest |
| `root.size` | The image size in bytes. It must fit into the slot (768 MiB) |
| `root.sha256` | The SHA-256 checksum of the image |
| `boot` | Optional: the boot image that goes with the root image. Without it, the update copies the running boot image into the other slot |
| `boot.url`, `boot.size`, `boot.sha256` | As for `root`. The image must fit into the boot partition (16 MiB) |

`just release` writes the manifest for you. The image versions of your own
builds have the form `YEAR.MONTH.DAY.HOURMINUTE-COMMIT`. A release's version is
its tag without the `v`.

## What's next

- If an update doesn't come up, see
  [Recover from a failed update](/docs/recovery/#recover-from-a-failed-update).
- For the technical details of slots and verification, see
  [Architecture](/docs/architecture/).
