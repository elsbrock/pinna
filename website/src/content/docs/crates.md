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
| `proto/` | The definition of the control API (`pinna/v1/device.proto`) and of standalone mode's setup API (`pinna/v1/assistant.proto`) |
| `kernel/` | The kernel source reference (`source.env`), Pinna's patches and configuration, and the build script that packages the kernel as a Debian package. `kernel/firmware/` holds the microphone FPGA bitstream |
| `dashboard/` | The dashboard, written in Svelte. `pinnad` serves it |
| `services/updates/` | The release service behind `updates.getpinna.dev`, a Cloudflare Worker in TypeScript: update channels and release downloads (ADR 0022) |
| `releases/` | The notes of each release, which the `release-notes` skill drafts |
| `firmware/` | The MT8163 Wi-Fi and Bluetooth firmware: Amazon's files, unmodified |
| `mkosi/` | The image definition: Debian packages, configuration files and systemd units that go into the root filesystem |
| `scripts/` | Build helpers, `flash-gen2.sh` for installing over fastboot, `backup-gen2.sh` and `verify-backup.sh` for the device backup, `fetch-models.sh` and `push-dev.sh` |
| `tools/wmt/` | Small C programs that power up the Wi-Fi and Bluetooth chip |
| `keys/` | The public key that root filesystem images and update manifests must be signed with |
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
| `pinnad` | The voice satellite: wake word, buttons, light ring, requests to Home Assistant or, in standalone mode, to your own services, timers, announcements, media playback, mute and volume. Also serves the control API, the dashboard and the MCP server, and checks the update channel |
| `pinna-esphome` | The server side of the ESPHome native API, with or without encryption |
| `pinna-wakeword` | Wake word detection for `.onnx` and `.tflite` models (the openWakeWord words and compatible classifiers), run on the `tract` inference library |
| `pinna-audio` | Microphone capture and speaker playback, through ALSA or PipeWire, with mixing, ducking and audio decoding |
| `pinna-ladspa` | The microphone beamformer, as a LADSPA plugin for PipeWire |
| `pinna-hal` | Access to the LED ring, the buttons, the microphone privacy switch and the device identity, with mock versions for tests |
| `pinna-api` | The control API service, generated from `proto/` |
| `pinnactl` | The command-line client for the control API, the update installer, and the update check that `pinnad` runs (`pinnactl update --check`) |
| `pinna-bootctl` | The fastboot request and the A/B slot state in the `misc` partition, and the boot stages on the ring that `pinna-init` and `pinna-bootstages` share |
| `pinna-bootstages` | Shows the last boot stages on the ring (USB, Wi-Fi, `pinnad`) after the system starts, then hands the ring to `pinnad` (ADR 0012) |
| `pinna-init` | The program in the initramfs: chooses a slot, checks its signature, sets up dm-verity and starts the system |
| `pinna-update` | Checks a release's signed manifest and the channel's rules, and writes its images into the inactive slot |
| `pinna-airplay` | Runs `shairport-sync` and passes its audio, the sender's volume and the sender's track to `pinnad` (ADR 0011) |
| `pinna-bt` | Opens the Bluetooth pairing window in BlueZ, accepts pairings only then, reports the paired phones, and passes a phone's audio (BlueALSA), volume and track to `pinnad` (ADR 0011) |
| `pinna-bridge` | What `pinna-airplay` and `pinna-bt` share: sessions on the stream socket and the control API on `127.0.0.1` |
| `pinna-assist` | One standalone request, without device state (ADR 0017): the adapters for speech-to-text (OpenAI-compatible and ElevenLabs), the brain (OpenAI-compatible chat completions, Anthropic's Messages API and Gemini) and text-to-speech (OpenAI-compatible and ElevenLabs), the presets, the checks a connection gets when it is saved, the tool loop, the sentence splitter, the system prompt and the format of `standalone.toml` |
| `pinna-mcp` | The MCP protocol (ADR 0017): JSON-RPC messages and tool types, the server logic behind `pinnad`'s `/mcp`, and the Streamable HTTP client for your own MCP servers |
| `pinna-vad` | The energy VAD and the end-of-speech detector, without dependencies. The wake word detection and standalone requests use it (ADR 0017) |
| `pinna-http` | The HTTP clients that the updater, the media player and standalone mode share, with one TLS setup: rustls on ring, trusting the image's CA certificates (ADR 0017) |
| `pinna-version` | The version the binaries report: `PINNA_VERSION` at build time, which the release workflow sets to the release's version (ADR 0022) |

### Standalone mode in `pinnad`

These modules of `pinnad` run [standalone mode](/docs/standalone/) (ADR 0017):

| Module | Purpose |
| --- | --- |
| `standalone/` | Owns `standalone.toml` and `secrets.toml`: checks and saves each change and hands the result to the state machine (`service.rs`), starts each request as a task of its own (`spawn.rs`), and keeps the spoken acknowledgment for a slow agent (`ack.rs`) |
| `voice/local.rs` | A standalone request inside the state machine: its phase and deadlines, the conversation, the reply on the voice lane, the last failure, typed questions and spoken samples |
| `voice/mode.rs` | The switch between Home Assistant and standalone mode, and the Home Assistant commands that standalone mode ignores |
| `voice/timers.rs` | The timers that Home Assistant sets, and the device's own, which it saves in `timers.toml` |
| `voice/messages.rs` | Messages from the other members of the satellite group (ADR 0026): said at once or held, at most 5 for 12 hours each, and tried once more after their speech fails |
| `tools/` | The tools that the brain and MCP clients call: the device's controls, timers, the time, the weather from Open-Meteo, radio, updates, and, for the brain only, messages to the other members of the satellite group (`send_message`) and the same controls on another member (`remote.rs`) |
| `mcp.rs` | The MCP server at `/mcp` on the dashboard's listener |
| `assistant_api.rs` | `AssistantService`, the setup API that the dashboard's **Assistant** page uses |
| `group/` | The satellite group (ADR 0026): the member list's rules (`model.rs`), the group's id, key and signatures (`seal.rs`), the group as `standalone.toml` and `secrets.toml` keep it (`persisted.rs`), what the **Assistant** page shows of it (`view.rs`), the signed calls between members (`envelope.rs`, `client.rs`, and `peer.rs`, which serves `PeerService`), what is known of the other members while `pinnad` runs, with Add, Remove and leaving (`runtime.rs`), and the exchange of member lists after every search (`exchange.rs`) |
| `mic.rs` | Sends the microphone audio to Home Assistant or to a standalone request, starting 200 ms before the end of the wake word |
| `checks.rs` | The checks on a command's values that the control API and the tools share |
| `store.rs` | Writes `settings.toml` and the standalone files durably, and keeps the entries this version can't read |
| `secrets.rs` | Reads and writes `secrets.toml`, which only its owner can read (mode 0600) |
| `clock.rs` | The local time, from the time zone setting, for the time tool and the brain |

### On your computer

| Crate | Purpose |
| --- | --- |
| `pinna-image` | Seals and signs root filesystem images, creates signing keys, writes and signs update manifests, verifies images, and compares image versions |
| `pinna-bootimg` | Builds and inspects Android boot images and the boot level they provide, and builds the initramfs |

`pinnad` runs the voice satellite in one process. The beamformer runs inside
PipeWire, and `pinna-init` runs as the first process in the initramfs. The
crates keep the parts separate, so that they can run as separate processes
later if needed.

## What's next

- How the parts fit together: [Architecture](/docs/architecture/)
- How to build and test them: [Contribute](/docs/development/)
