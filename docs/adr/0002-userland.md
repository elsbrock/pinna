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
| `pinna-wakeword` | openWakeWord on pure-Rust `tract`, matching onnxruntime (scores within 1e-3 in the parity test; ~3e-6 measured) |
| `pinna-hal` | IS31FL3236 LED ring (kernel sysfs), evdev buttons, privacy latch; mocks |
| `pinna-audio` | On the Echo Dot `aplay` for the speaker and `pw-record` for the processed microphone stream (`arecord` in the ALSA fallback); `pw-record`/`pw-play` elsewhere; DSP front end, mixer sequences, decoding |
| `pinna-bootctl` | Bootloader message (fastboot guard), A/B slot state (`slot-status`, `mark-good`, `set-active`) |
| `pinna-bootimg` | Boot image and initramfs tooling |
| `pinna-init` | Initramfs PID 1: A/B slot choice, image signature, dm-verity root, switch root (ADR 0003) |
| `pinna-image` | Signed slot images: seal, verify, manifest, keys (host tool + library) |
| `pinna-update` | Installs a release into the inactive slot (library behind `pinnactl update`) |
| `pinna-ladspa` | LADSPA plugin running the microphone front end (beamformer) inside PipeWire (ADR 0004) |

- **One process for now.** Pure-Rust inference removed the main reason for a
  separate voice process (C++ crash isolation). Crates keep the seams to split
  later.
- **Audio (superseded by ADR 0004)**: on the Echo Dot the microphone path
  (beamformer, WebRTC echo cancellation) runs in a system-wide PipeWire graph
  and pinnad drives the speaker through ALSA, feeding PipeWire the echo
  reference; laptops use the PipeWire backend, the VM images none.
- **Home Assistant integration is the ESPHome native API** — HA adopts the
  device with no YAML; mDNS through systemd-resolved `.dnssd`.
- **Local control API is ConnectRPC**, without authentication. On
  `127.0.0.1:7470` it serves `pinnactl` (the console client) and the boot
  health check. A second listener serves the same service to the LAN, with the
  dashboard (ADR 0005).
