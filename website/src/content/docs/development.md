---
title: Contribute
description: Set up a development environment, run the tests, try changes on a device, and send them upstream.
---

This page explains how to work on Pinna: how to set up the tools, which tests
exist and what each one needs, and how to log in to a device and try a change on
it. Owners set up and use a satellite on its [dashboard](/docs/dashboard/);
the developer pages cover the rest: this page, [Build Pinna](/docs/build/),
the [developer settings](/docs/configure/) in `pinnad.toml`,
[`pinnactl` and the control API](/docs/control-api/), and
[Releases and updates](/docs/releases/).

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
[Build Pinna](/docs/build/#add-your-own-files). The tests in this section don't, unless
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
| `just sendspin-conformance` | Music Assistant's own Sendspin server library (`aiosendspin` 10.2.0 and 10.3.0, as Music Assistant 2.10.7 and 2.11.0b9 pin it) plays to a `pinnad` on your computer: the handshake, PCM and FLAC at 48 and 44.1 kHz played on the server's timeline, a new track and a format change mid-stream, volume and mute both ways, track details, pause, play and stop through the group, leaving the group, reconnecting, a second server, a new name and switching off | `uv`, and network access the first time, to download the Python packages. It builds `pinnad` and gives it free ports, so a development `pinnad` can keep running |
| `just wakeword-parity` | Pinna's wake word scores match the reference implementation (onnxruntime) on test recordings | The wake word models (`just models`); skips without them |
| `just e2e` | A complete voice request: `pinnad` with the real wake word and a recording, with a simulated Home Assistant | The wake word models (`just models`); skips without them |
| `just web-check` | The dashboard: design tokens, generated API code, unit tests and `svelte-check`. `just test` runs it | Network access the first time, to download the npm packages |
| `just kernel-config` | The kernel configurations contain every option Pinna needs: the MT8163 device, the MT8516 Echos (planned), and QEMU on ARM, arm64 and x86_64 | Network access the first time, to download the kernel source |
| `just bootimg-roundtrip` | The boot image unpacks and repacks byte for byte | `build/pinna-boot.img` from `just bootimg` |
| `just vm-test` | The real ARM root image boots in QEMU, and SSH and the Home Assistant API come up | `just kernel-qemu image` first. Takes a few minutes, because QEMU emulates the CPU |
| `just vm-test-arm64` | The same ARM root image boots on the arm64 kernel, as 32-bit programs: the way the MT8516 Echos will run it (planned) | `just kernel-qemu-arm64 image` first. Slower than `just vm-test`, because QEMU emulates a 64-bit CPU. `just test` doesn't run it |
| `just vm-test-x86` | The same checks with the x86_64 image, and the virtual Echo Dot: its buttons, microphone button, speaker, and the wake word through its microphone | `just kernel-x86 image-x86` first. Faster with KVM. The wake word check needs the models (`just models`); skips without them |
| `just vm-hotspot-test` | The setup hotspot, with four simulated Wi-Fi radios in the x86_64 virtual machine: a phone in a network namespace joins it, is sent to the dashboard and scans there; a wrong password tried from the phone stops the hotspot, brings it back and shows why; a simulated home network kept from its new address keeps it off; forgetting it brings the hotspot back | `SSH_KEY=~/.ssh/key just vm-hotspot-test`, after `just kernel-x86 image-x86` with that key's public half in `secrets/authorized_keys`. `just test` doesn't run it |
| `just vm-rescue-test` | The initramfs stays in rescue mode, without restarting, on a disk without a root filesystem image and on one without slot partitions | `just kernel-qemu` first. `just vm-rescue-test-x86` runs the same on x86_64 and needs `just kernel-x86`. `just test` doesn't run either |
| `just vm-update-test` | An update installs into the other slot, the trial boot succeeds, and a broken update rolls back | `just kernel-qemu image` and the signing key. Builds two more images, so it takes longer |
| `just updates-test` | The release service (`services/updates/`): its channels, file downloads, cache, withdrawn releases, and the counting and the statistics, against recorded GitHub answers | Node 24 or later; no Cloudflare or GitHub account |
| `just test-device` | Speaker, microphones, LED ring and the Home Assistant API on a real device | A running device reachable over SSH |

`just wakeword-parity` and `just e2e` pass without running anything if the
wake word models are missing. Run `just models` before you rely on their
result.

The experimental Echo Dot (5th gen) port mounts the existing UBIFS
`userdata` volume as `/var`. To check the mount units generated for MT8519,
GPT, and virtual machines, run this on your computer with Python 3, systemd,
and a writable `/sys` mount:

```sh
python3 tests/vm/test-var-mount.py
```

After building the MT8519 kernel, run its register and IRQ regressions on your
computer:

```sh
python3 tests/kernel/test-mt8519-sram.py
python3 tests/kernel/test-mt8519-i2c.py
python3 tests/kernel/test-mt8519-thermal.py
python3 tests/kernel/test-mt8519-pinctrl.py
python3 tests/kernel/test-mt8519-audio.py
python3 tests/kernel/test-mt8519-privacy.py
python3 tests/kernel/test-mt8519-light.py
python3 tests/kernel/test-mt8519-sdio.py
python3 tests/kernel/test-mt8519-watchdog.py
python3 tests/kernel/test-mt8519-codec-links.py
python3 tests/kernel/test-mt8519-adc-clock.py
python3 tests/kernel/test-mt8519-afe-dram.py
python3 tests/kernel/test-mt8519-mclk.py
python3 tests/kernel/test-mt8519-etdm-params.py
python3 tests/kernel/test-mt8519-bt-wait.py
python3 tests/kernel/test-mt8519-one-stream.py
```

They need `cc` and the patched source in `build/linux`. They compile the actual
driver helpers with recording register, IRQ, GPIO and input interfaces. They
also check device-tree references against the drivers that consume them. They
check the ADSP_INFRA offset and acknowledgement failures, FIFO transfers and DMA
selection, and device-tree IRQ lookup and errors. They do not access a satellite.

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

## Try a change without a device

`just dev-vm` runs the x86_64 image in a virtual machine as a **virtual Echo
Dot**. Your browser stands in for its hardware: its microphone, its speaker
and its four buttons. The image, `pinnad`, the wake word and Home Assistant's
voice pipeline are the same as on a Dot. The Echos' microphone stream, with
its echo canceller, is not part of it.

You need:

- KVM: write access to `/dev/kvm`, usually by being in the `kvm` group.
  Without it the CPU is emulated, and the satellite takes many minutes to
  start.
- Podman or Docker, installed on your computer. The development shell
  doesn't include them.
- The signing key and `secrets/authorized_keys`, as for any image (see
  [Build Pinna](/docs/build/#add-your-own-files)). Without `authorized_keys`, SSH and
  `just push-vm` can't log in.

```sh
just models                 # once: the wake word and speaker models
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

Without a satellite or the virtual machine, you can still work on the
dashboard's layout and text. This serves the dashboard with made-up data for
an Echo Dot (2nd gen) named Kitchen:

```sh
cd dashboard && npm run dev:mock
```

The mock backend keeps its state in memory. The pages show Wi-Fi, updates,
logs, the Assistant setup and the other settings, and a change you make, such
as the volume or a switch, shows at once. An action that needs a real
satellite, such as a restart, an update or joining a Wi-Fi network, answers
that the mock backend doesn't do it. Restarting `npm run dev:mock` resets the
data.

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
connect only one of them to it at a time.

## Work on one part

Most work happens in one crate. Some useful commands:

```sh
cargo test -p pinna-wakeword                      # tests of one crate
cargo run -p pinna-bootimg -- info build/pinna-boot.img
```

To find your way around the code, see [Repository layout](/docs/crates/).

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
- Offer your own builds as updates, or publish a release:
  [Releases and updates](/docs/releases/).
- Read about the [architecture](/docs/architecture/).
