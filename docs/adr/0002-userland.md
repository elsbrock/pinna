# ADR 0002: Userland — one Rust daemon on standard services

Status: accepted (2026-11-15)

## Decision

Standard Debian/systemd services do everything generic (network, mDNS, SSH,
time, logging, watchdog, USB gadget via configfs). Pinna's own code is the
`pinnad` daemon plus small tools:

| Crate | Role |
|---|---|
| `pinnad` | State machine: wake word / button → Home Assistant Assist run → TTS; timers, announcements, mute, LED light entity, volume; serves the local control API |
| `pinna-esphome` | ESPHome native API server, plaintext or Noise (NNpsk0, key provisioning by HA), conformance-tested against aioesphomeapi |
| `pinna-api` | ConnectRPC `pinna.v1.DeviceService` (proto in `proto/`), generated with `connectrpc-build` + `protox` |
| `pinnactl` | Console client for the control API (`status`, `volume`, `mute`, `listen`, `stop`, `led-test`, `watch`) and the A/B updater (`update`) |
| `pinna-wakeword` | openWakeWord on pure-Rust `tract`, matching onnxruntime to 3e-6 |
| `pinna-hal` | IS31FL3236 LED ring (kernel sysfs), evdev buttons, privacy latch; mocks |
| `pinna-audio` | Capture/playback via `arecord`/`aplay` (device) or `pw-record`/`pw-play`; DSP front end, mixer sequences, decoding |
| `pinna-bootctl` | Bootloader message (fastboot guard), A/B slot state (`slot-status`, `mark-good`, `set-active`) |
| `pinna-bootimg` | Boot image and initramfs tooling |
| `pinna-init` | Initramfs PID 1: A/B slot choice, image signature, dm-verity root, switch root (ADR 0003) |
| `pinna-image` | Signed slot images: seal, verify, manifest, keys (host tool + library) |
| `pinna-update` | Installs a release into the inactive slot (library behind `pinnactl update`) |

- **One process for now.** Pure-Rust inference removed the main reason for a
  separate voice process (C++ crash isolation). Crates keep the seams to split
  later.
- **Audio goes straight to ALSA on the device for v0.** PipeWire is installed
  but unused until the MT8163 card has a UCM profile and the amp sequence is
  modelled; laptops/VMs use the PipeWire backend.
- **Home Assistant integration is the ESPHome native API** — HA adopts the
  device with no YAML; mDNS through systemd-resolved `.dnssd`.
- **Local control API is ConnectRPC** on `127.0.0.1:7470`, unauthenticated and
  therefore localhost-only; `pinnactl` is the console client and the boot health
  check. A web UI (Svelte + connect-es) can use the same protos later.
