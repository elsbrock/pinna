---
title: Overview
description: What Pinna is, what it does on an Echo Dot (2nd gen), and where the project stands right now.
---

Pinna is an open, image-based Linux system for the **Echo Dot (2nd gen)**
(MediaTek MT8163) and, later, the rest of the Echo (2nd gen) family. It turns
the device into a **Home Assistant voice satellite**: the wake word, the LED
ring and the buttons run on the device, and audio goes to your Home Assistant
over the ESPHome native API — no cloud, no account, nothing to Amazon.

It replaces the previous LibreEcho firmware with a standard userland: Debian
trixie built with mkosi, systemd, a read-only erofs root on `system_a`, a
persistent `/var` on `userdata`, and small Rust daemons. The kernel keeps the
MT8163 board support from LibreEcho-Linux and needs no initramfs.

> **Status: early bring-up.** Console only. Expect sharp edges. The image
> builds and passes its VM boot gates; bring-up and tuning on real hardware is
> in progress.

## What you get

- **Home Assistant adoption without YAML.** Pinna speaks the ESPHome native
  API and announces itself over mDNS. Entity IDs are carried over from
  LibreEcho, so existing dashboards and voice pipelines keep working. See
  [Home Assistant](/docs/home-assistant/).
- **On-device wake word.** openWakeWord ("Alexa") running on pure-Rust
  `tract`, parity-tested against onnxruntime.
- **LED ring, buttons, mute.** The ring is a light entity with effects; the
  buttons surface as events; the privacy mute and volume are first-class
  controls.
- **A local control API.** ConnectRPC on `127.0.0.1:7470` with a console
  client, `pinnactl`. See [Control API](/docs/control-api/).
- **A way back.** A fastboot guard on every boot and a one-script restore.
  See [Recovery](/docs/recovery/).

## Where to start

1. [Build](/docs/build/) the kernel, root filesystem and boot image.
2. [Flash](/docs/flash/) the Echo Dot over fastboot.
3. [Adopt](/docs/home-assistant/) the device in Home Assistant.

Background reading: the [architecture](/docs/architecture/) and the
[crates](/docs/crates/) that make it up. If you want the honest list of what
is missing, see [Status & roadmap](/docs/status/).
