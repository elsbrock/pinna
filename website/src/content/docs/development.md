---
title: Contribute
description: Set up a development environment, run the tests, try changes on a device, and send them upstream.
---

This page explains how to work on Pinna: how to set up the tools, which tests
exist and what each one needs, and how to try a change on a running device.

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

   Its version, such as `0.4.1-dev.202611170910.g1a2b3c4` (or `0.0.1-dev.…`
   while no release is tagged), is newer than the release it builds on and
   than older builds of yours.

2. On the satellite, over SSH, add the address to
   `/var/lib/pinna/pinnad.toml` and restart `pinnad`:

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
