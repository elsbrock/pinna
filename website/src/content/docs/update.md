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

1. `pinnactl update` checks the signature of the release's manifest, then
   downloads the new boot image and root filesystem image and writes them to
   the slot that isn't running. It checks each download against its checksum
   in the manifest. After writing, it reads the
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

Pinna only installs releases whose manifest and images are signed with the
key whose public part is built into the device: the project key
`keys/release.pub` in a release, `secrets/release.pub` in a build of your own.
Build your updates with the same `secrets/release.key` that you used for the
first installation.

## How the satellite finds updates

The **Updates** setting chooses where the satellite looks for new releases:

| Setting | What the satellite does |
| --- | --- |
| **Stable** | Looks for finished releases. Release images start with this setting |
| **Beta** | Also looks for betas, releases before they are finished, and moves on to a finished release when that is newer. A beta from the browser installer starts with this setting |
| **Off** | Doesn't look. Builds of your own start with this setting |

With **Stable** or **Beta**, the satellite asks `updates.getpinna.dev` for
the newest release on its channel: once after it starts, as soon as its clock
is set, then once a day at a time it picked at random, when you change the
setting, and when you select **Check now**. It fetches the release's manifest
and its signature, nothing else, and checks the signature. Its request carries
no identifier. Nothing is downloaded or installed until you install it.

A newer release then shows on the dashboard's **System** page and in Home
Assistant. The satellite never goes back to an older version on its own, and a
satellite on **Stable** never installs a beta.

No release is published yet, and `updates.getpinna.dev` doesn't answer yet.
Until it does, **System** shows `updates.getpinna.dev did not answer`, and you
install builds of your own as described in
[Install a build of your own](#install-a-build-of-your-own).

## Before you begin

- The device runs Pinna, and its current slot is marked `good`. Pinna refuses
  to install an update while the running slot is still on trial. The dashboard
  says so on **System**; on the device, `pinna-bootctl slot-status` shows it.
- The device runs a Pinna built on or after 17 November 2026. An older one
  keeps its own slot record, which the new version doesn't read: move it over
  once, as in [Move a device from an older Pinna](#move-a-device-from-an-older-pinna).

## Install an update from the dashboard

1. In the [dashboard](/docs/dashboard/), open **System**. Under **Updates**,
   choose **Stable** or **Beta** if the setting is **Off**.
2. Select **Check now** to look at once. A newer release shows below it, with
   a summary and a link to its release notes.
3. Select **Install**. The satellite installs the release that the page shows,
   and no other: if a newer one appeared since, the page asks you to look
   again.

   The page shows each step. The device keeps running the update if you close
   the page; open **System** again to see where it is. If the update fails,
   the page says **The update was not installed** and gives the reason, and
   **Output on the satellite** shows every line. The device keeps running the
   current version.
4. When the update is installed, select **Restart now**. The page reconnects
   when the device is back, and shows the new version on trial.

   To start it later instead, select **Start it later**. The update stays
   in its slot as **Not started yet**, and **System** offers **Start it**,
   which makes it start on the next restart without downloading it again.
5. Wait until the new slot is marked **Good**, within 5 minutes of the restart.
   You can install the next update after that.

If the new version doesn't start, the device goes back to the previous one.
**System** then says, for example, `0.4.1 failed to start and was rolled
back.`, shows the new slot as **Can't boot**, and doesn't offer that version
again. A newer release is offered as usual. See
[Recover a device](/docs/recovery/) if the device doesn't come back at all.

**System** says why **Install** isn't there when an update can't be
installed:

- **Needs a new boot image: install it over USB.** The release needs a newer
  kernel or `pinna-init` than the one that would start it. See
  [Update the boot image](#update-the-boot-image).
- **This release is for …, not for ….** The release was published for another
  model.
- **The running version is still on trial.** Wait until it is marked **Good**.
- **A development overlay is active.** Programs from `just push` would hide
  the new release's programs. Remove them with `scripts/push-dev.sh --remove`,
  on your computer.

## Install an update from Home Assistant

The **Firmware** update entity shows the running version and the newest
release on the satellite's channel, with the release's summary and a link to
its notes. **Install** installs that release, and Home Assistant shows its
progress.

- Home Assistant has no restart button for the satellite, so after an install
  from Home Assistant the satellite restarts on its own. It waits until it is
  Idle, with no timer set and nothing playing. To keep the new version back,
  select **Start it later** on the dashboard before then.
- **Skip** belongs to Home Assistant; the satellite doesn't store it.
- Until a check has succeeded since the satellite started, and while
  **Updates** is off, the entity's state is unknown: the satellite doesn't
  know whether it is up to date. A failed check doesn't change the entity.
- When the update can't be installed, the entity's summary says why, as on
  the dashboard. **Install** then does nothing.
- The **Updates** select in Home Assistant is the same setting as on the
  dashboard.

## Go back to an older version

Going back is your choice; the satellite never does it on its own. Your
settings stay: the older version keeps the settings it doesn't know, and they
apply again after the next update.

- When the other slot holds the older version, switch to it, as in
  [Switch to the other version](#switch-to-the-other-version).
- When the channel's release is older than the running version, for example
  after you changed **Updates** from **Beta** to **Stable**, **System** offers
  **Install … (older)**. It asks before it installs.

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

## Install a build of your own

A satellite without internet access, or one that runs your own builds, installs
a release folder from your network. Before you begin, the device must reach
your computer over Wi-Fi or over the USB cable.

1. On your computer, build the update files:

   ```sh
   just release
   ```

   The files are in `build/release/`: the root image, the boot image,
   `manifest.json` and its signature `manifest.json.sig`. `just release`
   replaces them each time, so copy a release that works to a safe place.
   A release from GitHub has the same files.

2. Serve the folder over HTTP, on your computer:

   ```sh
   python3 -m http.server 8000 --directory build/release
   ```

3. Install it, in one of these ways:

   - In the dashboard, open **System**, open **Install from a manifest URL**,
     check the address, and select **Install**. The address is filled in with
     port 8000 on the computer or phone that you opened the dashboard on, or
     with the last address you used.
   - On the device:

     ```sh
     pinnactl update http://192.168.1.20:8000/manifest.json --reboot
     ```

   Replace `192.168.1.20` with your computer's address. Over the USB cable,
   use the address that your computer got from the device, in the range
   `10.42.0.10` to `10.42.0.29`.

   An install from an address installs what it is given after the signature
   checks, an older version too, and says when it is older. `pinnactl` prints
   each step: the slot it installs into, the boot image
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
   a: good attempts=0 (priority 14, tries 0), version 0.4.0
   b: good attempts=0 (priority 15, tries 0), version 0.4.1-dev.202611170910.g1a2b3c4
   booted: b (version 0.4.1-dev.202611170910.g1a2b3c4)
   kernel: from boot_b
   bootloader: loads the kernel of the slot it starts
   ```

   `active` is the slot that starts next. `attempts` counts the boots a slot
   on trial has left; the bootloader stores one try more. `kernel` shows the
   boot partition the running kernel came from.

To give the new version more or fewer attempts than three, add
`--tries <number>` to `pinnactl update`.

A build of your own accepts only images signed with your key, so releases from
`updates.getpinna.dev` don't install on it. To check a server of your own on
the satellite's schedule, see
[Offer your own builds as updates](/docs/development/#offer-your-own-builds-as-updates).

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

A release that needs something new from the kernel or `pinna-init` says so in
its manifest (`requires.boot_level`). When the boot image that would start it
is older, the satellite shows the release with **Needs a new boot image:
install it over USB** and doesn't install it. Install the release's boot image
with `scripts/flash-gen2.sh boot`, then install the update.

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
`file://` URL, or from a local path. Next to it, at its address after
redirects, `manifest.json.sig` holds an ed25519 signature over the manifest's
exact bytes, made with the release key. Pinna checks the signature before it
reads anything in the manifest, and refuses a manifest without one.

```json
{
  "version": "0.4.0",
  "channel": "stable",
  "summary": "Quiet speech from across the room now wakes the satellite.",
  "notes_url": "https://github.com/elsbrock/pinna/releases/tag/v0.4.0",
  "requires": { "boot_level": 1 },
  "root": {
    "url": "pinna-0.4.0.root.img",
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
| `version` | The image version, a semantic version. It must match the version inside the image |
| `channel` | `stable`, or `beta` for a version with a prerelease part such as `0.4.0-beta.1` |
| `summary` | One line of at most 255 characters, for the dashboard and Home Assistant |
| `notes_url` | Optional: the release notes |
| `requires.boot_level` | The lowest boot level the root needs. Each boot image says which level it provides (`pinna.level=` in its header) |
| `requires.models` | Optional: the models the release is for, `echo-dot-2` or `echo-2`. Without it, every model |
| `root.url` | Where to download the root image. A relative URL is relative to the manifest's address after redirects |
| `root.size` | The image size in bytes. It must fit into the slot (768 MiB) |
| `root.sha256` | The SHA-256 checksum of the image |
| `boot` | Optional: the boot image that goes with the root image. Without it, the update copies the running boot image into the other slot |
| `boot.url`, `boot.size`, `boot.sha256` | As for `root`. The image must fit into the boot partition (16 MiB) |

`just release` writes the manifest and its signature for you. Releases have
semantic versions: `0.4.0`, or `0.4.0-beta.1` for a beta. Builds of your own
are versioned like `0.4.1-dev.202611170910.g1a2b3c4`: the next patch after the
newest release, then the time of the build in UTC and the commit, so they
sort after the release they build on and before the next one.

## What's next

- If an update doesn't come up, see
  [Recover from a failed update](/docs/recovery/#recover-from-a-failed-update).
- For the technical details of slots and verification, see
  [Architecture](/docs/architecture/).
