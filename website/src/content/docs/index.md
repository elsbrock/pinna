---
title: Overview
description: What Pinna is, how it works, and where to start.
---

Pinna is an open-source operating system for the Echo Dot (2nd gen). It turns
the device into a voice satellite for [Home Assistant](https://www.home-assistant.io/):
you say the wake word, the device sends your request to your Home Assistant,
and it plays the reply. The wake word detection runs on the device, and
Pinna doesn't use Amazon's services.

> **Early development.** Pinna works, but you build and install it yourself,
> and you control it from the command line, from the
> [dashboard](/docs/dashboard/) and from Home Assistant. See
> [Status and roadmap](/docs/status/).

## How it works

1. Pinna listens for a wake word on the device — Alexa unless you choose
   others. Until it hears one, no audio leaves the device.
2. It streams your request to Home Assistant over the ESPHome API. Your Assist
   pipeline turns the speech into text, handles the request and creates the
   spoken reply.
3. The device plays the reply. The light ring shows each step.

Home Assistant also gets the light ring, the buttons, mute, volume, timers and
a media player as entities, so you can use them in automations.

Underneath, Pinna is Debian with systemd, on a read-only root filesystem.
Updates go to a second copy of the system and roll back on their own if the
new version doesn't start.

## Where to start

To install Pinna, follow these pages in order:

1. [What you need](/docs/requirements/): the device, the files and the tools.
2. [Build Pinna](/docs/build/): build the images.
3. [Install Pinna](/docs/flash/): install them over fastboot.
4. [Connect to Home Assistant](/docs/home-assistant/): add the device and
   choose an Assist pipeline.

If something goes wrong, see [Recover a device](/docs/recovery/) and
[Troubleshooting](/docs/troubleshooting/).

## Learn more

- [Goals](/docs/goals/): what Pinna sets out to do, and what it doesn't.
- [Architecture](/docs/architecture/): how the system starts and runs.
- [Contribute](/docs/development/): work on Pinna.
