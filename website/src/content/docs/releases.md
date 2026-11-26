---
title: Releases and updates
description: Install an update over SSH, offer your own builds as updates, read the update manifest, run the release service, and publish a release.
---

Owners install updates from the dashboard or Home Assistant, as
[Update Pinna](/docs/update/) describes, and builds of their own from the
dashboard's **Install from a manifest URL**. This page covers the rest, for developers.

## Install an update over SSH

On the satellite, `pinnactl update` installs a release from its manifest
address. Serve the release folder from your computer as in
[Install a build of your own](/docs/update/#install-a-build-of-your-own), then
run on the satellite:

```sh
pinnactl update http://192.168.1.20:8000/manifest.json --reboot
```

Replace `192.168.1.20` with your computer's address. Over the USB cable, use
the address that your computer got from the device, in the range `10.42.0.10`
to `10.42.0.29`.

`pinnactl` prints each step: the slot it installs into, the boot image
(`kernel from … written to …`, or why the kernel stays), the root filesystem
download and its checksum, the signature and the hash tree, and which slot
the device starts next. Without `--reboot`, the update waits for the next
restart. To give the new version more or fewer attempts than three, add
`--tries <number>`. [pinnactl](/docs/control-api/#pinnactl) lists the other
options.

After the device restarts, check the result, on the satellite:

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

`active` is the slot that starts next. `attempts` counts the boots a slot on
trial has left; the bootloader stores one try more. `kernel` shows the boot
partition the running kernel came from. `bootloader` shows what Pinna knows
about the bootloader: whether it loads the kernel of the slot it starts (see
[Update the boot image](/docs/update/#update-the-boot-image)).

## Offer your own builds as updates

A build with your own key (`just keygen`) installs only images signed with
that key, and starts with **Updates** off. To give a satellite your builds the
way a channel gives it releases, with the dashboard's **Install** and Home
Assistant's update entity:

1. On your computer, build a release and serve it:

   ```sh
   just release
   python3 -m http.server 8000 --directory build/release
   ```

   Its version, such as `0.4.1-dev.202611170910.g1a2b3c4`, is newer than the
   release it builds on and than older builds of yours.

2. On the satellite, over SSH, add the address to
   `/var/lib/pinna/pinnad.toml` ([`[update]`](/docs/configure/#update)) and
   restart `pinnad`:

   ```toml
   [update]
   manifest = "http://192.168.1.20:8000/manifest.json"
   ```

   ```sh
   systemctl restart pinnad
   ```

3. After each `just release`, select **Check now** on **System > Updates**, then
   **Install**.

The address replaces the channel: **System > Updates** shows it instead of the
**Updates** setting, and Home Assistant's **Updates** select has it as its only
option. The satellite checks it on the same schedule as a channel (once after
it starts, once a day, and on **Check now**), once its clock is set from the
internet, and offers the build when its version is newer than the running one.
To go back to a channel, remove the lines and restart `pinnad` again.

`just release` puts `PINNA_SUMMARY`, `PINNA_NOTES_URL` and `PINNA_MODELS`
(models separated by spaces) into the manifest it signs, on your computer.
Without `PINNA_MODELS`, the release is for every model, so it needs the Echo
Dot (5th gen)'s kernel too (`just kernel-mt8519`); `just release` then builds
its boot image with `just fit`. To build for the Echo Dot (2nd gen) only:

```sh
PINNA_SUMMARY="The wake word interrupts a reply." PINNA_MODELS="echo-dot-2" just release
```

## Move a device from an older Pinna

A Pinna built before 17 November 2026 keeps the slot state in its own record,
and doesn't write the boot image in updates. Move it over once. This keeps
your settings, but the new version has no previous version to fall back to
until its next update.

1. On the device, over SSH, install the update without restarting:

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

## The update manifest

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
  "requires": { "boot_level": 1, "models": ["echo-dot-2", "echo-2", "echo-dot-5"] },
  "root": {
    "url": "pinna-0.4.0.root.img",
    "size": 157286400,
    "sha256": "…"
  },
  "boot": {
    "url": "pinna-boot.img",
    "size": 16777216,
    "sha256": "…"
  },
  "boots": [
    {
      "models": ["echo-dot-5"],
      "url": "pinna-boot-mt8519.itb",
      "size": 9437184,
      "sha256": "…"
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `version` | The image version, a semantic version. It must match the version inside the image |
| `channel` | `stable`, or `beta` for a version with a prerelease part such as `0.4.0-beta.1` |
| `summary` | One line of at most 255 characters, for the dashboard and Home Assistant |
| `notes_url` | Optional: the release notes |
| `requires.boot_level` | The lowest boot level the root needs. Each boot image says which level it provides (`pinna.level=` in an Android image's header, `pinna,boot-level` in a FIT's root node) |
| `requires.models` | The models the release is built for: `echo-dot-2`, `echo-2` or `echo-dot-5`. A manifest from before the Echo Dot (5th gen) may leave it out, which meant every model then |
| `root.url` | Where to download the root image. A relative URL is relative to the manifest's address after redirects |
| `root.size` | The image size in bytes. One root serves every model, so it must fit into the smallest system slot of `requires.models`: 768 MiB for the Echo Dot (2nd gen) and the Echo (2nd gen), 131,088,384 bytes for the Echo Dot (5th gen) |
| `root.sha256` | The SHA-256 checksum of the image |
| `boot` | Optional: the Android boot image that goes with the root image, for the Echo Dot (2nd gen) and the Echo (2nd gen). Satellites from before `boots` read only this. Without it, the update copies the running boot image into the other slot |
| `boot.url`, `boot.size`, `boot.sha256` | As for `root`. The image must fit into the boot partition (16 MiB) |
| `boots` | Optional: a boot image for each boot format the release carries. Each entry has `models` (the models it is for), `url`, `size` and `sha256`. The Echo Dot (5th gen) boots a U-Boot FIT, not an Android boot image. A satellite takes the entry that names its model, and otherwise `boot`. It refuses an image of the wrong format or for another board |
| `boots[].size` | As for `boot.size`, against the smallest boot slot of the entry's models: 16 MiB, or 12,697,600 bytes for the Echo Dot (5th gen) |

`just release` writes the manifest and its signature for you. It refuses a
release in which a model has no boot image of its format, or an image is
larger than the smallest slot of the models it is for. `pinna-image manifest`,
which it runs on your computer, takes `--models`, `--boot` for the Android
image and `--boot-for <models>=<path>` for each other image, such as
`--boot-for echo-dot-5=build/pinna-boot-mt8519.itb`. Releases have
semantic versions: `0.4.0`, or `0.4.0-beta.1` for a beta. Builds of your own
are versioned like `0.4.1-dev.202611170910.g1a2b3c4`: the nearest release tag
before the commit with its patch raised by one (after a beta tag such as
`v0.5.0-beta.1`, that tag's `0.5.0`; without any tag, `0.0.1`), then the time
of the build in UTC and the commit. So they sort after the release they build
on and before the next one.

## The release service

`services/updates/` is the Cloudflare Worker behind `updates.getpinna.dev`.
It reads the releases from GitHub, sends satellites to the newest one on their
channel, and serves its files to the browser installer. To run it on your
computer:

```sh
cd services/updates
npx wrangler dev
```

For the private repository, put a read-only GitHub token into
`services/updates/.dev.vars` as `GITHUB_TOKEN=…`. Its README says how to
deploy it and how to withdraw a release.

## Publish a release

1. Draft the release notes with the `release-notes` skill, for example
   "draft the notes for 0.4.0". It writes `releases/0.4.0.md` from the commits
   since the previous release. Read and edit it; its first paragraph is the
   summary that Home Assistant shows.
2. Commit the notes, tag the release and push the tag, on your computer:

   ```sh
   git tag v0.4.0
   git push origin HEAD v0.4.0
   ```

   Tags are `v<major>.<minor>.<patch>`, or `v<major>.<minor>.<patch>-beta.<n>`
   for a beta; the release workflow refuses any other tag, and a tag without
   its notes.
3. The workflow builds and signs the release and leaves it as a draft on
   GitHub, a beta as a prerelease. Publish it there; satellites on its channel
   find it at their next check.

## What's next

- [Build Pinna](/docs/build/) to get images and update files.
- For the technical details of slots and verification, see
  [Architecture](/docs/architecture/).
