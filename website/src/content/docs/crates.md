---
title: Repository layout
description: The folders of the Pinna repository and the Rust crates in it, with what each one is for.
---

This page helps you find your way around the
[Pinna repository](https://github.com/elsbrock/pinna).

## Folders

| Path | Contents |
| --- | --- |
| `crates/` | Pinna's Rust code, as one Cargo workspace. See [Crates](#crates) |
| `proto/` | The definition of the control API (`pinna/v1/device.proto`) |
| `kernel/` | The kernel source reference (`source.env`), Pinna's patches and configuration, and the build script that packages the kernel as a Debian package. `kernel/firmware/` holds the microphone FPGA bitstream |
| `dashboard/` | The dashboard, written in Svelte. `pinnad` serves it |
| `firmware/` | The MT8163 Wi-Fi and Bluetooth firmware: Amazon's files, unmodified |
| `mkosi/` | The image definition: Debian packages, configuration files and systemd units that go into the root filesystem |
| `scripts/` | Build helpers, `flash-gen2.sh` for installing over fastboot, `backup-dot.sh` and `verify-backup.sh` for the device backup, `fetch-models.sh` and `push-dev.sh` |
| `tools/wmt/` | Small C programs that power up the Wi-Fi and Bluetooth chip |
| `keys/` | The public key that root filesystem images must be signed with |
| `tests/` | Virtual machine tests (`vm/`), the end-to-end voice test (`e2e/`) and the hardware test (`device/`) |
| `design/` | The design system for the website and the dashboard |
| `vendor/` | Third-party code kept in the repository: the `.tflite` support of the `tract` inference library, with the fixes it needs (ADR 0013) |
| `website/` | This website |
| `docs/` | Decision records (`adr/`) and the first-flash runbook |
| `secrets/`, `blobs/` | Your own files (`secrets/`), and the downloaded wake word models and an optional old boot image for `just bootimg-roundtrip` (`blobs/`). Git ignores both. See [What you need](/docs/requirements/) |

## Crates

### On the device

| Crate | Purpose |
| --- | --- |
| `pinnad` | The voice satellite: wake word, buttons, light ring, requests to Home Assistant, timers, announcements, media playback, mute and volume. Also serves the control API and the dashboard |
| `pinna-esphome` | The server side of the ESPHome native API, with or without encryption |
| `pinna-wakeword` | Wake word detection for `.onnx` and `.tflite` models (the openWakeWord words and compatible classifiers), run on the `tract` inference library |
| `pinna-audio` | Microphone capture and speaker playback, through ALSA or PipeWire, with mixing, ducking and audio decoding |
| `pinna-ladspa` | The microphone beamformer, as a LADSPA plugin for PipeWire |
| `pinna-hal` | Access to the LED ring, the buttons, the microphone privacy switch and the device identity, with mock versions for tests |
| `pinna-api` | The control API service, generated from `proto/` |
| `pinnactl` | The command-line client for the control API, and the update installer |
| `pinna-bootctl` | The fastboot request and the A/B slot state in the `misc` partition |
| `pinna-init` | The program in the initramfs: chooses a slot, checks its signature, sets up dm-verity and starts the system |
| `pinna-update` | Writes a signed image into the inactive slot |

### On your computer

| Crate | Purpose |
| --- | --- |
| `pinna-image` | Seals and signs root filesystem images, creates signing keys and update manifests, and verifies images |
| `pinna-bootimg` | Builds and inspects Android boot images, and builds the initramfs |

`pinnad` runs the voice satellite in one process. The beamformer runs inside
PipeWire, and `pinna-init` runs as the first process in the initramfs. The
crates keep the parts separate, so that they can run as separate processes
later if needed.

## What's next

- How the parts fit together: [Architecture](/docs/architecture/)
- How to build and test them: [Contribute](/docs/development/)
