---
title: Goals
description: What Pinna sets out to do, what comes later, and what is out of scope.
---

These goals guide how Pinna is designed and what gets built next. When a
change works against one of them, the change needs a strong reason.

## Goals

### Keep speech local

Pinna detects the wake word on the Echo Dot itself. Until the device hears the
wake word, no audio leaves it. After that, audio goes only to the Home
Assistant instance that you connected the device to, over your own network.
Pinna doesn't use Amazon's services or any other cloud service to handle your
voice.

### Be a good Home Assistant voice satellite

Home Assistant adds a Pinna device the same way it adds any ESPHome device,
without YAML or add-ons. Everything the device can do is available in Home
Assistant: the light ring, the buttons, mute, volume, timers, announcements and
media playback.

### Be safe to install and update

Every installation step can be undone, and the documentation says how.
Fastboot stays reachable on every boot. Updates are signed, go to a second
system slot, and roll back automatically if the new system doesn't start.

### Use standard parts

Pinna is Debian with systemd, built as a disk image with mkosi. Standard Linux
services handle networking, SSH, time and logging, so what you know from other
Linux systems applies. Pinna's own code is a small set of Rust programs.

### Make development pleasant

`nix develop` sets up every tool, and `just` runs every build and test task.
Most tests don't need hardware: the full image boots in a virtual machine, the
Home Assistant connection is tested against Home Assistant's own client
library, and a simulated Home Assistant runs the complete voice flow. To try a
change on a real device, `just push` installs new binaries without
reinstalling the system.

### Stay open

All of Pinna's code is in the
[public repository](https://github.com/elsbrock/pinna). Proprietary firmware,
such as the Wi-Fi and Bluetooth firmware, comes from your own device and is
never published with Pinna.

## Later

- **More devices.** Pinna supports the Echo Dot (2nd gen) today. The Echo (2nd
  gen) uses the same platform and is the next device. Other devices may follow.
- **Standalone mode.** Using the Echo Dot as a voice assistant without a Home
  Assistant instance. Pinna's parts are kept separate so that this stays
  possible.

## Out of scope

- **Replacing Alexa.** Pinna doesn't use Amazon's services, so Alexa features
  such as skills, shopping and calling aren't available.
- **Cloud accounts and remote access.** Pinna has no account and no cloud
  service. If you want to reach your devices from outside your home, set that
  up in Home Assistant.
- **A general-purpose Linux distribution.** Pinna is built to be a voice
  satellite, not a base for other uses of the device.

## What's next

- See what works today and what's missing in
  [Status and roadmap](/docs/status/).
- To help with any of these goals, see [Contribute](/docs/development/).
