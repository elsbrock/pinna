---
title: Status and roadmap
description: What works in Pinna today, what is known to be missing or rough, and what is planned next.
---

Pinna is in early development. You can build it, install it on an Echo
(2nd gen) or an Echo Dot (2nd gen) and use it with Home Assistant, or on its
own in [standalone mode](/docs/standalone/), and with its own dashboard. Parts
of it are still being tuned on real hardware.

## What works

These parts are covered by tests that run without hardware:

- The kernel, the signed root filesystem image and the boot image build from
  the repository. The kernel is Debian's 6.12 with the MT8163 board support
  ported to it.
- The image boots in a virtual machine, in both the ARM and the x86_64
  version, and its services, SSH, the Home Assistant API, the dashboard and
  the [Prometheus metrics](/docs/metrics/) come up.
- A device with no system it can start stays in
  [rescue mode](/docs/recovery/#rescue-mode) instead of restarting.
- An update installs a kernel and a root filesystem into the second slot and
  boots on trial, and a broken update rolls back to the previous slot, kernel
  included, automatically. The bootloader's part of this is tested on the
  Echo Dot (2nd gen) only.
- The device checks an update channel for a newer signed release, shows it on
  the dashboard and in Home Assistant, and installs the release it showed
  (tested against a simulated release service and in the x86_64 virtual
  machine).
- The release service's channels and downloads pass their tests against
  recorded GitHub answers. The browser installer checks a release's signature
  and files in its tests.
- The Home Assistant connection passes tests with Home Assistant's own client
  library, including encryption and key rotation.
- The wake word detection gives the same results as the reference
  implementation, within 0.001.
- A complete voice request, from the wake word to the played reply, works
  against a simulated Home Assistant. The reply plays while it downloads, the
  wake word interrupts a reply and stops a ringing timer alarm, and changes on
  the dashboard reach Home Assistant.
- [Standalone mode](/docs/standalone/) runs a request against simulated
  services: a spoken or typed request goes through speech-to-text, the brain's
  tool loop or an agent, and text-to-speech that plays while it streams. The
  adapters for OpenAI-compatible services, Anthropic, Gemini and ElevenLabs,
  the save-time checks, the MCP server and client, device timers, the update
  tools and the dashboard's setup API pass their tests.
- [AirPlay](/docs/airplay/) turns on and off in the x86_64 virtual machine:
  the receiver starts, publishes its AirPlay record, reports that it is ready,
  and stops again.

On the devices:

- The microphone path with beamforming and echo cancellation runs; on the
  Echo Dot (2nd gen) it lowers the device's own music by 23 to 31 dB. See
  [Audio](/docs/audio/).
- `scripts/backup-gen2.sh` backs up the device, and `scripts/verify-backup.sh`
  checks the backup. See [What you need](/docs/requirements/#backup-of-the-device).
- A hardware test (`just test-device`) checks the speaker, the microphones,
  the light ring and the Home Assistant connection on a running device.
- Music plays over [AirPlay](/docs/airplay/) and
  [Bluetooth](/docs/bluetooth/) from phones and computers: turning a source on,
  pairing a phone, playing, the sender's volume, reconnecting, forgetting,
  turning it off, and starting after a restart.

## Known gaps

- **The dashboard has no login.** Anyone on your network can use it,
  including restarting and updating the device. See
  [Who can use the dashboard](/docs/dashboard/#who-can-use-the-dashboard).
- **No release is published yet.** The release workflow builds and signs the
  images and the manifest for a
  [release](https://github.com/elsbrock/pinna/releases), and
  `updates.getpinna.dev` serves them, but none has been published. Until then,
  devices report that there is no release yet, and you
  [build your own](/docs/build/).
- **The wake words are fixed in the image.** Pinna listens for up to two of
  Alexa, Hey Jarvis, Hey Mycroft, Hey Rhasspy (openWakeWord) and Okay Nabu at
  once, chosen in Home Assistant or on the dashboard. The openWakeWord models'
  licence doesn't allow commercial use, and the dashboard can't accept extra
  models yet.
- **On a Kaeru from before 22 October 2026, updates don't change the kernel.**
  That Kaeru loads `boot_a` for both slots, and updates leave `boot_a`
  alone. Install new kernels from fastboot.
- **No automated test runs.** The tests run when contributors run them; there
  is no CI for them yet. The release workflow builds the images but doesn't
  run the tests.
- **Standalone mode is new.** Its tests run against simulated services. It
  has been tried on an Echo Dot (2nd gen) with OpenRouter and ElevenLabs, not
  yet with each hosted service.
  - MCP servers that need a sign-in or a token in a header can't be added.

## Planned

- **Automatic installs** inside a time window that you choose, for releases
  that need nothing done by hand.
- **Custom wake words from the dashboard**: uploading an `.onnx` or `.tflite`
  model and naming its phrase (ADR 0013).
- **More for standalone mode**: MCP servers that need a sign-in, and several
  satellites that share one conversation (ADR 0017).
- **64-bit ARM**: the processor supports it, but nobody has started on it.
- **Multi-room with Music Assistant** through its own protocol, Sendspin: the
  device would play in step with other speakers in a group.

These plans follow the project's [goals](/docs/goals/).

## How you can help

- **Test it on a device.** Problems show up on real hardware first. Report
  what you see in an [issue](https://github.com/elsbrock/pinna/issues), with
  the version and the log from the dashboard, as
  [Report a problem](/docs/troubleshooting/#report-a-problem) describes.
- **Send changes.** See [Contribute](/docs/development/).
- **Bring up a new device.** Supporting another Echo device means work on the
  device tree, clocks, audio, microphones and LED ring, and a device to test
  on. Open an issue with the hardware you have.
