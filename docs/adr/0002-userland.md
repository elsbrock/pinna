# ADR 0002: Userland — one Rust daemon on standard services

Status: accepted (2026-11-15)

## Decision

Standard Debian/systemd services do everything generic (network, mDNS, SSH,
time, logging, watchdog, USB gadget via configfs). Pinna's own code is the
`pinnad` daemon plus small tools:

| Crate | Role |
|---|---|
| `pinnad` | State machine: wake word / button → Home Assistant Assist run → TTS; timers, announcements, mute, LED light entity, volume |
| `pinna-esphome` | ESPHome native API server (plaintext; Noise later), conformance-tested against aioesphomeapi |
| `pinna-wakeword` | openWakeWord on pure-Rust `tract`, matching onnxruntime to 3e-6 |
| `pinna-hal` | IS31FL3236 LED ring (kernel sysfs), evdev buttons, privacy latch; mocks |
| `pinna-audio` | Capture/playback via `arecord`/`aplay` (device) or `pw-record`/`pw-play`; DSP front end, mixer sequences, decoding |
| `pinna-bootctl` | Bootloader message (fastboot guard) |
| `pinna-bootimg` | Boot image tooling |

- **One process for now.** Pure-Rust inference removed the main reason for a
  separate voice process (C++ crash isolation). Crates keep the seams to split
  later.
- **Audio goes straight to ALSA on the device for v0.** PipeWire is installed
  but unused until the MT8163 card has a UCM profile and the amp sequence is
  modelled; laptops/VMs use the PipeWire backend.
- **Home Assistant integration is the ESPHome native API** — HA adopts the
  device with no YAML; mDNS through systemd-resolved `.dnssd`.
- Console-first: no web UI yet. The ConnectRPC API and `pinnactl` come next.
