---
title: Update Pinna
description: Install a new version of Pinna on a running device, and how the device falls back if the new version doesn't work.
---

Pinna keeps two copies of its root filesystem, in the partitions `system_a`
and `system_b`. An update goes into the copy that isn't running. The device
then tries the new version, and if the new version doesn't work, it goes back
to the old one on its own.

## How an update works

1. `pinnactl update` downloads the new root filesystem image and writes it to
   the slot that isn't running. It checks the download against the checksum in
   the update manifest. After writing, it checks the image's signature and its
   hash tree, which lets the device detect any changed block.
2. On the next boot, the device boots the new slot **on trial**, with three
   attempts.
3. When the new system is up, Pinna checks itself: `pinnad` must answer and
   SSH must run within five minutes. If the check passes, the slot is marked
   **good** and stays in use.
4. If the check fails or doesn't finish in time, the device restarts. Each
   boot on trial uses one attempt. When no attempts are left, the device
   returns to the previous slot and marks the new one as **unbootable**.

Pinna only installs images that are signed with the key whose public part is
built into the device: the project key `keys/release.pub` in a release,
`secrets/release.pub` in a build of your own. Build your updates with the
same `secrets/release.key` that you used for the first installation.

## Update to a new release

A release's `manifest.json` installs the newest published release. On the
device, with Wi-Fi and internet access:

```sh
pinnactl update https://github.com/elsbrock/pinna/releases/latest/download/manifest.json --reboot
```

It writes the new root filesystem into the other slot, checks it and restarts
into it, as described above. When the release also changes the boot image,
that still needs fastboot: download `pinna-boot.img` from the release and
flash it as in [Update the boot image](#update-the-boot-image).

## Before you begin

- The device runs Pinna, and its current slot is marked `good`. Pinna refuses
  to install an update while the running slot is still on trial. To check, run
  `pinna-bootctl slot-status` on the device.
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

   `pinnactl` prints each step: the download, the checksum, the signature and
   the hash tree, and which slot the device starts next. Without `--reboot`,
   the update waits for the next restart.

4. After the device restarts, check the result:

   ```sh
   pinna-bootctl slot-status
   ```

   Within a few minutes, the new slot is the running slot and marked `good`.

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

Updates only replace the root filesystem. The bootloader always boots
`boot_a`, so the boot image, which holds the kernel and the program that
chooses a slot, has no fallback, and `pinnactl update` doesn't change it. When a release changes the boot image,
install it from fastboot:

```sh
scripts/flash-gen2.sh boot
```

See [Install Pinna](/docs/flash/#install-the-boot-image).

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
  }
}
```

| Field | Meaning |
| --- | --- |
| `version` | The image version. It must match the version inside the image |
| `root.url` | Where to download the root image. A relative URL is relative to the manifest |
| `root.size` | The image size in bytes. It must fit into the slot (768 MiB) |
| `root.sha256` | The SHA-256 checksum of the image |

`just release` writes the manifest for you. Image versions have the form
`YEAR.MONTH.DAY.HOURMINUTE-COMMIT`.

## What's next

- If an update doesn't come up, see
  [Recover from a failed update](/docs/recovery/#recover-from-a-failed-update).
- For the technical details of slots and verification, see
  [Architecture](/docs/architecture/).
