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
   the update manifest, then checks the image signature and the dm-verity hash
   tree after writing.
2. On the next start, the device starts the new slot **on trial**, with three
   attempts.
3. When the new system is up, Pinna checks itself: `pinnad` must answer and
   SSH must run within five minutes. If the check passes, the slot is marked
   **good** and stays in use.
4. If the check fails or doesn't finish in time, the device restarts. Each
   start on trial uses one attempt. When no attempts are left, the device
   returns to the previous slot and marks the new one as **unbootable**.

Pinna only installs images that are signed with the key whose public part is
built into the device. Build your updates with the same `secrets/release.key`
that you used for the first installation.

## Before you begin

- The device runs Pinna, and its current slot is marked `good`. Pinna refuses
  to install an update while the running slot is still on trial. To check, run
  `pinna-bootctl slot-status` on the device.
- The device can reach your computer over the network, either over Wi-Fi or
  over the USB cable.

## Install an update

1. On your computer, build the update files:

   ```sh
   just release
   ```

   The files are in `build/release/`: the root image, the boot image and
   `manifest.json`.

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

## Update the boot image

Updates only replace the root filesystem. The boot image (`boot_a`), which
holds the kernel and the program that chooses a slot, has only one copy, so
Pinna doesn't update it automatically. When a release changes the boot image,
install it from fastboot:

```sh
scripts/flash-dot.sh boot
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
