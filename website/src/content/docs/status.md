---
title: Status and roadmap
description: What works in Pinna today, what is known to be missing or rough, and what is planned next.
---

Pinna is in early development. You can build it, install it on an Echo
(2nd gen) or an Echo Dot (2nd gen) and use it with Home Assistant, or on its
own in [standalone mode](/docs/standalone/), and with its own dashboard. Parts
of it are still being tuned on real hardware.

Until Pinna 1.0, how it works and how you set it up can change from one
release to the next, standalone mode most of all. The release notes say what
changed. What owners report shapes these changes; see
[How you can help](#how-you-can-help).

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
- With **Install updates** set to **Automatically**, the satellite installs a
  release by itself at a random minute in a window that you choose, and
  restarts in the window once it is Idle. Releases that need something by hand
  wait for you. Its tests run against a simulated release service; no device
  has installed a release this way yet.
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
  the dashboard reach Home Assistant. A wake word uploaded on the dashboard is
  tested, offered to Home Assistant and wakes the device.
- [Standalone mode](/docs/standalone/) runs a request against simulated
  services: a spoken or typed request goes through speech-to-text, the brain's
  tool loop or an agent, and text-to-speech that plays while it streams. The
  adapters for OpenAI-compatible services, Anthropic, Gemini and ElevenLabs,
  the save-time checks, the MCP server and client, device timers, the update
  tools and the dashboard's setup API pass their tests.
- In a [satellite group](/docs/satellite-groups/#pass-messages-between-satellites),
  satellites add and remove each other, pass messages and act on each other
  in tests that run several satellites in one process. No test has run on two
  devices yet.
- The [setup hotspot](/docs/flash/#connect-to-wi-fi) runs in the x86_64
  virtual machine with simulated Wi-Fi radios: a phone joins it, gets an
  address, is sent to the dashboard and scans for networks there; a join
  turns it off, a wrong password brings it back and the phone reads why, and
  a kept network keeps it off.
- [AirPlay](/docs/airplay/) turns on and off in the x86_64 virtual machine:
  the receiver starts, publishes its AirPlay record, reports that it is ready,
  and stops again.
- The [Sendspin](/docs/sendspin/) player, which is experimental, plays
  sessions with a test server over a WebSocket on the same computer: the
  encrypted handshake, audio on the server's timeline, volume and mute in
  both directions, a paused group and its controls, a rename, a second server
  taking over from the first, and refusing a browser or a third connection. In a simulation of the speaker, timed playback
  stays within 1 ms of the server's clock from 10 seconds after a stream
  starts.

On the devices:

- `scripts/backup-gen2.sh` backs up the device, and `scripts/verify-backup.sh`
  checks the backup. See [What you need](/docs/requirements/#backup-of-the-device).
- A hardware test (`just test-device`) checks the speaker, the microphones,
  the light ring and the Home Assistant connection on a running device.
- Music plays over [AirPlay](/docs/airplay/) and
  [Bluetooth](/docs/bluetooth/) from phones and computers: turning a source on,
  pairing a phone, playing, the sender's volume, reconnecting, forgetting,
  turning it off, and starting after a restart.

## Known gaps

- **The microphone stream hasn't run on a satellite yet.** It averages the
  seven microphones and cancels the satellite's own sound. On recordings from
  an Echo Dot (2nd gen) it removed about 26 dB of the satellite's music and
  speech. See [Audio](/docs/audio/).
- **The dashboard's password is optional, and it travels unencrypted.** Until
  you set one, anyone on your network can use the dashboard, including
  restarting and updating the device. The satellite serves plain HTTP, so
  someone who can read the traffic on your network can read the password too.
  A reverse proxy with TLS in front of the dashboard protects the path from
  your browser to the proxy; the hop from the proxy to the satellite stays plain
  HTTP over your network. See
  [Who can use the dashboard](/docs/dashboard/#who-can-use-the-dashboard).
- **The built-in wake words' licence doesn't allow commercial use.** Pinna
  listens for up to two of Alexa, Hey Jarvis, Hey Mycroft, Hey Rhasspy
  (openWakeWord) and Okay Nabu at once, and the openWakeWord models' licence
  doesn't allow commercial use. You can
  [add your own](/docs/wake-words/#add-your-own-wake-word).
- **The test of an uploaded wake word doesn't say whether it hears you.** The
  satellite checks that the model loads, keeps up and stays quiet on silence
  and noise, not that it wakes on its phrase. Models that read more than the
  standard 16 embeddings, which openWakeWord trains for some long phrases,
  are refused.
- **On a Kaeru from before 22 October 2026, updates don't change the kernel.**
  That Kaeru loads `boot_a` for both slots, and updates leave `boot_a`
  alone. Install new kernels from fastboot.
- **The setup hotspot hasn't run on an Echo yet**, and its password,
  `pinnasetup`, is the same on every device. While a device has no saved
  network, anyone in range who knows the password can join its hotspot, use
  its dashboard, and see what you type there.
- **No automated test runs.** The tests run when contributors run them; there
  is no CI for them yet. The release workflow builds the images but doesn't
  run the tests.
- **Standalone mode is new.** Its tests run against simulated services. It
  has been tried on an Echo Dot (2nd gen) with OpenRouter and ElevenLabs, not
  yet with each hosted service.
  - MCP servers that sign in through a browser (OAuth) can't be used.
- **Sendspin is experimental.** Its specification is still a draft. Music
  Assistant connects without pairing, so anyone on your network can play to
  the satellite. How closely the satellite keeps step with other speakers
  isn't measured on a device yet: the delay from the speaker's hardware
  clock to the sound counts as 0 until it is. See
  [Play music with Sendspin](/docs/sendspin/#limits).

## Planned

- **A score for each wake word**: the highest score of the last thing each
  word heard, so that you can tell a word that never wakes from one that
  nearly does.
- **More for standalone mode**: several satellites that share one
  conversation. In a satellite group,
  one answer when several hear the wake word.
- **A setup hotspot password that only the owner can learn**, instead of one
  that every device shares.
- **64-bit ARM**: the processor supports it, but nobody has started on it.
- **Pairing for Sendspin through Home Assistant**: Home Assistant would hand
  the satellite's pairing key to Music Assistant, so that only your Music
  Assistant could play to it.

These plans follow the project's [goals](/docs/goals/).

## How you can help

- **Say what you think.** Before 1.0, feedback changes the most. Say what
  works for you, what doesn't and what you miss, in
  [Discussions](https://github.com/elsbrock/pinna/discussions). Standalone
  mode needs it most.
- **Test it on a device.** Problems show up on real hardware first. Report
  what you see in an [issue](https://github.com/elsbrock/pinna/issues), with
  the version and the log from the dashboard, as
  [Report a problem](/docs/troubleshooting/#report-a-problem) describes.
- **Send changes.** See [Contribute](/docs/development/).
- **Bring up a new device.** Supporting another Echo device means work on the
  device tree, clocks, audio, microphones and LED ring, and a device to test
  on. [Echo devices](/docs/devices/) lists which ones can be unlocked. Open
  an issue with the hardware you have.
