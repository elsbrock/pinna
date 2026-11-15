---
title: Status & roadmap
description: An honest list of what works, what needs tuning, and what is not built yet.
---

Pinna is an early bring-up project, not a product. This page exists so nobody
flashes a device expecting a finished appliance.

> **Status: early bring-up.** Console only. Expect sharp edges.

## Working today

- The kernel builds and is packaged as a Debian `.deb`; config assertions run
  for every variant.
- The mkosi image builds (Debian trixie armhf, erofs root) and boots in QEMU;
  the x86_64 variant boots under KVM.
- The boot image round-trips byte-identically against the device's original
  `boot_a` backup.
- The ESPHome API passes conformance against `aioesphomeapi` 46.2.0,
  including Noise encryption and key rotation.
- Wake word scoring matches onnxruntime to 3e-6, and the full voice flow
  (wake word → ESPHome → simulated Home Assistant → TTS playback) runs in the
  end-to-end test.
- `pinnactl` drives the control API and acts as the boot health check.

## Needs on-device tuning

These are not testable off-device and defaults may be wrong until someone with
a Dot in hand tunes them:

- microphone gain and the wake word threshold (`/etc/pinna/pinnad.toml`,
  scores are logged by `pinnad`),
- the speaker mixer sequence,
- LED ring direction/ordering.

## Not built yet

- A web UI. The control API protos are ready for `connect-es`, but the console
  is the interface for now.
- **Standalone mode.** Today the voice path goes through a Home Assistant
  instance; using the Dot as a self-contained, open voice assistant is
  planned.
- A/B root updates: `system_a`/`system_b` with a per-slot kernel command line,
  plus dm-verity.
- A kernel on Debian's 6.12 LTS instead of the LibreEcho-Linux 6.1 fork.
- arm64 (the ATF supports it; nobody started).
- PipeWire on the device once the MT8163 card has a UCM profile.

## How to help

- **Bug reports.** This is early bring-up: it misbehaves on physical units
  first. Report what breaks with the build you flashed and the logs
  (`journalctl -u pinnad` shows wake word scores and pipeline state) at
  [GitHub issues](https://github.com/elsbrock/pinna/issues). Measurements from
  real hardware — mic gain, scores, mixer sequences — are welcome in any form.
- **Contributions.** The workspace is small and deliberately seamed: Rust
  daemons, systemd units, a thin kernel, a proto-first control API. Open an
  issue first if you plan something larger, so nobody duplicates work.
- **New devices and generations.** Supporting another Echo generation means a
  board bring-up (DTB, clocks, audio, microphones, LED ring) and a test unit.
  The [architecture](/docs/architecture/) page and the ADRs describe the boot
  chain and the constraints; start a discussion with what hardware you have.
