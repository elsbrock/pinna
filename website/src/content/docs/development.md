---
title: Contribute
description: Set up a development environment, run the tests, try changes on a device, and send them upstream.
---

This page explains how to work on Pinna: how to set up the tools, which tests
exist and what each one needs, how to log in to a device and try a change on
it, and how updates work for developers. Owners set up and use a satellite on
its [dashboard](/docs/dashboard/); the developer pages cover the rest: this
page, the [developer settings](/docs/configure/) in `pinnad.toml`, and
[`pinnactl` and the control API](/docs/control-api/).

Pinna is a small project in early development. Before you start on something
larger than a fix, open an issue so that the work isn't duplicated.

## Set up the tools

Pinna builds on an x86_64 Linux computer. [Nix](https://nixos.org/download/)
with flakes enabled is the easiest way to get the tools, but Nix is not
required: without it, install the same tools yourself (`flake.nix` is the
list). The repository's development shell provides the Rust toolchain, the
ARM cross compilers, mkosi, QEMU, fastboot,
[uv](https://docs.astral.sh/uv/) for the Python-based tests, and Node for the
dashboard (`just web`, `just image` and `just web-check`).

1. Clone the repository:

   ```sh
   git clone https://github.com/elsbrock/pinna.git
   cd pinna
   ```

2. Start the development shell, or skip this step and use the tools you
   installed yourself:

   ```sh
   nix develop
   ```

3. List the available tasks:

   ```sh
   just
   ```

Building images also needs a signing key and the files described in
[What you need](/docs/requirements/). The tests in this section don't, unless
the table says so.

## Run the checks

Run the host checks on every change. They format-check the code, run Clippy
with warnings as errors, and run the unit tests of every crate:

```sh
just check
```

To run everything, including the virtual machine tests, run:

```sh
just test
```

`just test` takes a while, and some of its parts need files that you build
first. The repository has no CI that runs these tests, so run them yourself
before you send a change that affects the image or the device.

## Tests and what they need

| Task | What it checks | What it needs |
| --- | --- | --- |
| `just check` | Formatting, Clippy and the unit tests of every crate | Nothing else |
| `just esphome-conformance` | Home Assistant's own client library (`aioesphomeapi` 46.2.0) talks to Pinna's ESPHome API server: the entity set, encryption, key rotation and error cases | Network access the first time, to download the Python packages |
| `just wakeword-parity` | Pinna's wake word scores match the reference implementation (onnxruntime) on test recordings | The wake word models (`just models`); skips without them |
| `just e2e` | A complete voice request: `pinnad` with the real wake word and a recording, with a simulated Home Assistant | The wake word models (`just models`); skips without them |
| `just web-check` | The dashboard: design tokens, generated API code, unit tests and `svelte-check`. `just test` runs it | Network access the first time, to download the npm packages |
| `just kernel-config` | The kernel configuration for the device, QEMU and x86_64 contains every option Pinna needs | Network access the first time, to download the kernel source |
| `just bootimg-roundtrip` | The boot image unpacks and repacks byte for byte | `build/pinna-boot.img` from `just bootimg` |
| `just vm-test` | The real ARM root image boots in QEMU, and SSH and the Home Assistant API come up | `just kernel-qemu image` first. Takes a few minutes, because QEMU emulates the CPU |
| `just vm-test-x86` | The same checks with the x86_64 image, and the virtual Echo Dot: its buttons, microphone button, speaker, and the wake word through its microphone | `just kernel-x86 image-x86` first. Faster with KVM. The wake word check needs the models (`just models`); skips without them |
| `just vm-rescue-test` | The initramfs stays in rescue mode, without restarting, on a disk without a root filesystem image and on one without slot partitions | `just kernel-qemu` first. `just vm-rescue-test-x86` runs the same on x86_64 and needs `just kernel-x86`. `just test` doesn't run either |
| `just vm-update-test` | An update installs into the other slot, the trial boot succeeds, and a broken update rolls back | `just kernel-qemu image` and the signing key. Builds two more images, so it takes longer |
| `just updates-test` | The release service (`services/updates/`): its channels, file downloads, cache and withdrawn releases, against recorded GitHub answers | Node 24 or later; no Cloudflare or GitHub account |
| `just test-device` | Speaker, microphones, LED ring and the Home Assistant API on a real device | A running device reachable over SSH |

`just wakeword-parity` and `just e2e` pass without running anything if the
wake word models are missing. Run `just models` before you rely on their
result.

## Log in to a device

Owners never need to log in: the [dashboard](/docs/dashboard/) has every
setting. To debug or develop, log in on your computer in one of these ways:

- **Over SSH**, with the key from `secrets/authorized_keys` that the image was
  built with. Pinna accepts only keys, no passwords. A release image has no
  key: add yours over the serial console first. Over the USB cable the device
  is at `10.42.0.1`, and your computer gets an address between `10.42.0.10`
  and `10.42.0.29`:

  ```sh
  ssh root@10.42.0.1
  ```

  Over Wi-Fi, use the device's IP address or its `.local` name, such as
  `pinna-2a914e.local`.
- **Over the USB serial console**, which needs no key. It also has a root
  shell in rescue mode, where SSH doesn't run:

  ```sh
  picocom -b 115200 /dev/ttyACM0
  ```

After a reinstall the device has a new SSH host key, and `ssh` refuses to
connect. Remove the old key, on your computer:

```sh
ssh-keygen -R 10.42.0.1
ssh-keygen -R pinna-2a914e.local   # your device's name
```

These commands, on the satellite, show what the dashboard shows and more:

```sh
pinnactl status            # the state, as in pinnactl and the control API
systemctl --failed         # services that failed
journalctl -b -u pinnad    # the log of pinnad since this start
pinna-bootctl slot-status  # the slots, their versions and states
```

Include their output when you [report a problem](/docs/troubleshooting/#report-a-problem).

## When pinnad doesn't start

`pinnad` refuses to start if `pinnad.toml` contains a setting it doesn't know,
a value of the wrong type, or an invalid encryption key file. Then the health
check fails, and the device restarts into fastboot (see
[The fastboot guard](/docs/recovery/#the-fastboot-guard)). To find the cause:

1. Start Pinna again, on your computer:

   ```sh
   fastboot reboot
   ```

2. Log in over the USB serial console right away: the health check restarts
   the device about 40 seconds after Pinna's services start. The serial
   console doesn't need an SSH key.
3. Read the log of the previous start, and fix the cause:

   ```sh
   journalctl -b -1 -u pinnad
   ```

If you can't fix it in time, install the root filesystem again from fastboot,
as described in [Recover a device](/docs/recovery/). This erases the settings,
which also removes a broken configuration.

## Try a change on a device

To test new versions of Pinna's programs on a running device without
reinstalling it, use `just push`:

```sh
just push
```

`just push` cross-compiles `pinnad`, `pinnactl`, `pinna-bootctl` and
`pinna-bootstages`, copies them to the device over SSH as a system extension
in `/var/lib/extensions/pinna-dev`, and restarts `pinnad`. The root filesystem
stays read-only and unchanged.

By default it connects to `root@10.42.0.1`, over USB. To use another address, set
`PINNA_HOST`:

```sh
PINNA_HOST=root@192.168.1.40 just push
```

To remove the extension and go back to the installed programs, run:

```sh
scripts/push-dev.sh --remove
```

While the extension is there, the satellite doesn't install updates: after the
restart, the extension's programs would hide the new release's. Remove it
first.

## Updates for developers

Owners install updates from the dashboard or Home Assistant, as
[Update Pinna](/docs/update/) describes, and builds of their own from the
dashboard's **Install from a manifest URL**. This section covers the rest.

### Install an update over SSH

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

### Offer your own builds as updates

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

3. After each `just release`, select **Check now** on **System**, then
   **Install**.

The address replaces the channel: **System** shows it instead of the
**Updates** setting, and Home Assistant's **Updates** select has it as its only
option. The satellite checks it on the same schedule as a channel (once after
it starts, once a day, and on **Check now**), once its clock is set from the
internet, and offers the build when its version is newer than the running one.
To go back to a channel, remove the lines and restart `pinnad` again.

`just release` puts `PINNA_SUMMARY`, `PINNA_NOTES_URL` and `PINNA_MODELS`
(models separated by spaces) into the manifest it signs, on your computer:

```sh
PINNA_SUMMARY="The wake word interrupts a reply." PINNA_MODELS="echo-dot-2" just release
```

### Move a device from an older Pinna

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

### The update manifest

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

## Try a change without a device

`just dev-vm` runs the x86_64 image in a virtual machine as a **virtual Echo
Dot**. Your browser stands in for its hardware: its microphone, its speaker
and its four buttons. The image, `pinnad`, the wake word and Home Assistant's
voice pipeline are the same as on a Dot. The beamformer and the Dot's echo
canceller are not part of it.

You need:

- KVM: write access to `/dev/kvm`, usually by being in the `kvm` group.
  Without it the CPU is emulated, and the satellite takes many minutes to
  start.
- Podman or Docker, installed on your computer. The development shell
  doesn't include them.
- The signing key and `secrets/authorized_keys`, as for any image (see
  [What you need](/docs/requirements/)). Without `authorized_keys`, SSH and
  `just push-vm` can't log in.

```sh
just models                 # once: the wake word models
just kernel-x86 image-x86   # once, and again after changes to the image
just dev-vm                 # keeps running; stop it with Ctrl-C
just dev-ha                 # in a second terminal
```

Without `just models` the image has no wake word, and saying it does
nothing.

`just dev-ha` runs a Home Assistant for the satellite on your computer, in
containers: Home Assistant, Whisper for speech to text and
Piper for text to speech. It sets them up once, with an Assist pipeline and
the virtual Echo Dot added. The first start downloads the images and the
speech models, a few gigabytes. Open it at `http://localhost:8123`; from this
computer it needs no login. `just dev-ha stop` removes the containers, and
`build/dev-ha/` keeps their state.

Then:

1. Open the dashboard at `http://127.0.0.1:18090`, on the same computer.
   Browsers share the microphone only with `localhost` or `127.0.0.1`. From
   another computer, forward the port with
   `ssh -L 18090:127.0.0.1:18090 <this computer>` and open the same address
   there.
2. On the Overview, under **Virtual Echo Dot**, select **Connect** and allow
   the browser to use the microphone.
3. Say "Alexa, what time is it?" The ring on the Overview is the satellite's
   own: it shows what `pinnad` lights.

Hold the round buttons the way you would on the Dot: a short press, a long
press, or **+** held down to repeat. The microphone button turns the
microphones off and on, like the Dot's.

**Audio file as the microphone** (**Choose…**, once connected) plays a
recording into the satellite instead of your voice. That gives the same input every time, for example
`crates/pinna-wakeword/tests/fixtures/alexa-twice.wav`.

The virtual machine keeps its disk in `build/dev-vm/`, so Home Assistant
keeps knowing it after a restart. Each start installs the newest
`build/pinna-x86.root.img`. `FRESH=1 just dev-vm` starts with an empty
`/var`. The virtual satellite has its own identity. It never joins Home
Assistant as your Echo Dot.

To try a new `pinnad` without building the image again, run `just push-vm`
while the virtual machine is running. It works like `just push`, but the
programs it puts on the satellite last only until the virtual machine
restarts. Then the image's own are back.

To work on the dashboard with live reloading, point Vite at the virtual
machine:

```sh
cd dashboard && PINNA_URL=http://127.0.0.1:18090 npm run dev
```

Open the `localhost` address Vite prints. The virtual Echo Dot works there too.

To use your own Home Assistant instead of `just dev-ha`, start the virtual
machine with `ESPHOME_BIND=0.0.0.0 just dev-vm`. Then add the satellite under
**Settings › Devices & services › Add integration › ESPHome**, with this
computer's address and port `6053`. Your computer's firewall and your network
must let Home Assistant reach that port. Discovery doesn't work from inside
the virtual machine.

To run a second virtual machine, for example from another worktree, give it
other ports: `WEB_PORT=18190 SSH_PORT=2323 ESPHOME_PORT=6153 just dev-vm`, and
`SSH_PORT=2323 just push-vm` to update it. There is one `just dev-ha` per
computer: running it from another worktree replaces the containers. Images
built from the same checkout are the same device to Home Assistant, so
connect only one of them to it at a time. The decision record is
[ADR 0014](https://github.com/elsbrock/pinna/tree/main/docs/adr/0014-virtual-echo-dot.md).

## Work on one part

Most work happens in one crate. Some useful commands:

```sh
cargo test -p pinna-wakeword                      # tests of one crate
cargo run -p pinna-bootimg -- info build/pinna-boot.img
```

To find your way around the code, see [Repository layout](/docs/crates/). For
the reasons behind the design, read the decision records in
[`docs/adr/`](https://github.com/elsbrock/pinna/tree/main/docs/adr).

## Send a change

1. Open an issue that describes the problem or the feature.
2. Make the change on a branch. Write commit messages in the
   [Conventional Commits](https://www.conventionalcommits.org/) style that the
   history uses, for example `fix(pinnad): …` or `feat(image): …`.
3. Run `just check`, and the tests from the table that cover what you changed.
4. Open a pull request on [GitHub](https://github.com/elsbrock/pinna) and say
   which tests you ran.

## Licences

Pinna's own code is under the MIT licence. The Linux kernel is under the GNU
GPL version 2. The wake word models that `just models` downloads are under
CC BY-NC-SA 4.0, which doesn't allow commercial use. They are never committed
to the repository. If you build an image with them, the image contains them,
so keep that licence in mind before you share it.

## What's next

- [Build Pinna](/docs/build/) to get images you can install.
- Read about the [architecture](/docs/architecture/).
