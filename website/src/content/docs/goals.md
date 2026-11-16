---
title: Goals
description: What Pinna sets out to do, what comes later, and what is out of scope.
---

These goals guide how Pinna is designed and what gets built next. When a
change works against one of them, the change needs a strong reason.

## Goals

### Your voice goes only where you send it

Pinna detects the wake word on the device itself. Audio leaves the device only
after the wake word, or when someone starts a request on purpose: with the
Action button, from the dashboard, or from Home Assistant. The light ring shows
whenever the device is listening. The audio
goes to your Home Assistant, or in standalone mode to the services set up on
the device: hosted services with your own keys, or your own servers. Pinna
doesn't use Amazon's services, has no cloud service of its own, and never picks
a service for you. In standalone mode, anyone who can use the dashboard can
change those services; see
[Who can use the dashboard](/docs/dashboard/#who-can-use-the-dashboard).

### Be a good Home Assistant voice satellite

Home Assistant adds a Pinna device the same way it adds any ESPHome device,
without YAML or add-ons. Everything the device can do is available in Home
Assistant: the light ring, the buttons, mute, volume, timers, announcements and
media playback.

### Play music without getting in the way

The device can be a speaker for phones and computers too, over AirPlay and
Bluetooth. The music goes through the same audio path as everything
else, so the wake word keeps working while it plays, and replies,
announcements and timers always come first. Each source is off until you turn
it on.

### Work without Home Assistant

In standalone mode the device is a voice assistant on its own. It sends your
requests to the services you choose, from hosted models to your own servers,
or to an AI agent such as Hermes or OpenClaw. Agents can also use the device as
their voice and speaker. You set everything up on the dashboard. Standalone
mode is planned; see [Status and roadmap](/docs/status/).

### Be safe to install and update

Every installation step can be undone, and the documentation says how.
Fastboot stays reachable on every boot. Updates are signed, go to a second
system slot, and roll back automatically if the new system doesn't start.
The device looks for new releases once a day and shows them on the dashboard
and in Home Assistant; none installs without you.

### Use standard parts

Pinna is Debian with systemd, built as a disk image with mkosi. Standard Linux
services handle networking, SSH, time and logging, so what you know from other
Linux systems applies. Pinna's own code is a small set of Rust programs.

### Make development pleasant

`nix develop` sets up every tool — Nix is convenient, not required — and
`just` runs every build and test task.
Most tests don't need hardware: the full image boots in a virtual machine, the
Home Assistant connection is tested against Home Assistant's own client
library, and a simulated Home Assistant runs the complete voice flow. To try a
change on a real device, `just push` installs new binaries without
reinstalling the system.

### Stay open

All of Pinna's code is in the
[public repository](https://github.com/elsbrock/pinna). Proprietary firmware,
such as the Wi-Fi firmware, is included unmodified in `firmware/`. It is kept
apart from Pinna's own code and isn't under Pinna's license.

## Later

- **More devices.** Pinna supports the Echo (2nd gen) and the Echo Dot
  (2nd gen). Other Echo devices may follow.

## Out of scope

- **Replacing Alexa.** Pinna doesn't use Amazon's services, so Alexa features
  such as skills, shopping and calling aren't available.
- **A Pinna account or remote access.** Pinna has no account. The one service
  of the project that a device contacts is `updates.getpinna.dev`, to look for
  new releases: once a day and after each start. It learns what any web server
  learns, the address a request comes from and when, and the channel the
  device asks for (`stable` or `beta`), nothing else. Its logs keep these for
  a few days, at Cloudflare, which runs it. Set **Updates** to **Off** on the
  dashboard and the device contacts no service of the project. The services
  you connect in standalone mode are yours. If you want to reach your devices
  from outside your home, set that up in Home Assistant or on your own
  network.
- **A general-purpose Linux distribution.** Pinna is built to be a voice
  assistant, not a base for other uses of the device.

## What's next

- See what works today and what's missing in
  [Status and roadmap](/docs/status/).
- To help with any of these goals, see [Contribute](/docs/development/).
