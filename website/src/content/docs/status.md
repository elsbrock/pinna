---
title: Status and roadmap
description: What works in Pinna today, what is known to be missing or rough, and what is planned next.
---

Pinna is in early development. You can build it, install it on an Echo Dot
(2nd gen) and use it with Home Assistant and its own dashboard, but there is no
prebuilt image, and parts of it are still being tuned on real hardware.

## What works

These parts are covered by tests that run without hardware:

- The kernel, the signed root filesystem image and the boot image build from
  the repository. The kernel is Debian's 6.12 with the MT8163 board support
  ported to it.
- The image boots in a virtual machine, in both the ARM and the x86_64
  version, and its services, SSH and the Home Assistant API come up.
- An update installs into the second slot and boots on trial, and a broken
  update rolls back to the previous slot automatically.
- The Home Assistant connection passes tests with Home Assistant's own client
  library, including encryption and key rotation.
- The wake word detection gives the same results as the reference
  implementation, within 0.001.
- A complete voice request, from the wake word to the played reply, works
  against a simulated Home Assistant.

On the Echo Dot itself:

- The microphone path with beamforming and echo cancellation runs, and lowers
  the device's own music by 23 to 31 dB. See [Audio](/docs/audio/).
- `scripts/backup-dot.sh` backs up the device, and `scripts/verify-backup.sh`
  checks the backup. See [What you need](/docs/requirements/#backup-of-the-device).
- A hardware test (`just test-device`) checks the speaker, the microphones,
  the light ring and the Home Assistant connection on a running device.

## Known gaps

- **The dashboard has no login.** Anyone on your network can use it,
  including restarting and updating the device. See
  [Who can use the dashboard](/docs/dashboard/#who-can-use-the-dashboard).
- **No prebuilt images.** You build your own images and sign them with your
  own key.
- **One wake word.** Pinna listens for "Alexa", with the openWakeWord model.
  The model's licence doesn't allow commercial use.
- **The boot image isn't updated automatically.** Updates replace the root
  filesystem only. A new kernel needs fastboot.
- **No automated test runs.** The tests run when contributors run them; there
  is no CI for them yet.

## Planned

- **The Echo (2nd gen)**, which uses the same platform as the Echo Dot.
- **Standalone mode**: using the device as a voice assistant without Home
  Assistant.
- **64-bit ARM**: the processor supports it, but nobody has started on it.

These plans follow the project's [goals](/docs/goals/).

## How you can help

- **Test it on a device.** Problems show up on real hardware first. Report
  what you see, with the image version, the output of `pinnactl status` and the
  log (`journalctl -b -u pinnad`), in an
  [issue](https://github.com/elsbrock/pinna/issues).
- **Send changes.** See [Contribute](/docs/development/).
- **Bring up a new device.** Supporting another Echo device means work on the
  device tree, clocks, audio, microphones and LED ring, and a device to test
  on. Open an issue with the hardware you have.
