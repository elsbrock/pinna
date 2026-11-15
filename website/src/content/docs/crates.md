---
title: Crates & layout
description: The Rust workspace, the image and kernel directories, and what each piece is responsible for.
---

## Repository layout

| Path | What |
| --- | --- |
| `crates/` | Rust workspace: the daemon, the ESPHome API, wake word, HAL, audio, boot tooling |
| `kernel/` | Kernel config fragment and build (packaged as a Debian `.deb`) |
| `mkosi/` | Debian trixie image definition |
| `proto/` | ConnectRPC definition of the local control API |
| `scripts/` | Flash, backup verification and model fetching |
| `docs/` | Decision records and the first-flash runbook |
| `secrets/`, `blobs/` | Owner-local inputs — never committed (except as documented templates) |
| `website/` | This site (Astro + Tailwind, published on GitHub Pages) |

## Crates

| Crate | Role |
| --- | --- |
| `pinnad` | The daemon: state machine (wake word / button → Assist run → TTS), timers, announcements, mute, LED light entity, volume; serves the local control API |
| `pinna-esphome` | ESPHome native API server — plaintext or Noise (NNpsk0, key provisioning by Home Assistant), conformance-tested against `aioesphomeapi` |
| `pinna-api` | ConnectRPC `pinna.v1.DeviceService` (generated from `proto/` with `connectrpc-build` + `protox`) |
| `pinnactl` | Console client for the control API — also the boot health check |
| `pinna-wakeword` | openWakeWord v0.5.1 models on pure-Rust `tract`, matching onnxruntime to 3e-6 |
| `pinna-hal` | IS31FL3236 LED ring (kernel sysfs), evdev buttons, privacy latch; mocks for tests |
| `pinna-audio` | Capture/playback via `arecord`/`aplay` (device) or `pw-record`/`pw-play`; DSP front end, mixer sequences, decoding |
| `pinna-bootctl` | Bootloader message (the fastboot guard) |
| `pinna-bootimg` | Android boot image tooling (pack, info, round-trip verification) |

The pure-Rust wake word removed the original reason for a separate voice
process (C++ crash isolation), so one process runs everything for now — the
crate boundaries keep the seams for splitting later.
