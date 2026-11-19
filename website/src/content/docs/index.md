---
title: Overview
description: What Pinna is, its two modes, and where to start.
---

Pinna is an open-source operating system for the Echo (2nd gen) and the Echo
Dot (2nd gen). It turns the device into a voice satellite: you say the wake
word, and the device sends your request to your own
[Home Assistant](https://www.home-assistant.io/), or, in standalone mode, to
the speech services and the language model or AI agent you choose. The wake
word detection runs on the device, and Pinna doesn't use Amazon's services.

> **Early development.** Pinna works, but you install it yourself over
> fastboot. See [Status and roadmap](/docs/status/).

## How it works

1. Pinna listens for a wake word on the device: Alexa, unless you choose
   others. Until it hears one, no audio leaves the device.
2. It sends your request to the assistant you chose, which turns the speech
   into text, handles the request and creates the spoken reply.
3. The device plays the reply. The light ring shows each step.

You set up the satellite on its [dashboard](/docs/dashboard/), a web page that
the device serves to your network. Every setting is there, in both modes.

## Two modes

On the dashboard's **Assistant** page you choose who answers requests. You can
switch at any time.

| | Home Assistant mode | Standalone mode |
| --- | --- | --- |
| What answers | Your Assist pipeline in Home Assistant | Speech-to-text, a brain and text-to-speech that you set up on the dashboard. The brain is a language model or an AI agent such as Hermes or OpenClaw |
| What you need | Home Assistant on your network | An account with a hosted service, or your own servers on your network |
| Timers | Home Assistant's, shown on the ring | The satellite's own, shown on the ring |
| Automations | The ring, the buttons, mute, volume, timers and a media player are Home Assistant entities | Your AI agent can use the satellite over MCP. Home Assistant can stay connected for its entities |
| Set it up | [Connect to Home Assistant](/docs/home-assistant/) | [Use Pinna without Home Assistant](/docs/standalone/) |

A new satellite starts in Home Assistant mode.

Both modes have the same ring and buttons, the wake word tuning, music over
[AirPlay](/docs/airplay/) and [Bluetooth](/docs/bluetooth/), and
[updates](/docs/update/) that go back to the previous version if the new one
doesn't start.

## Where to start

To install and set up Pinna, follow these pages in order:

1. [What you need](/docs/requirements/): the device, the files and the tools.
2. Get the images: download them from a
   [release](https://github.com/elsbrock/pinna/releases), or
   [build them](/docs/build/).
3. [Install Pinna](/docs/flash/): install them over fastboot, and connect the
   device to Wi-Fi.
4. [Use the dashboard](/docs/dashboard/): open it, and name the device.
5. Choose who answers: [connect to Home Assistant](/docs/home-assistant/), or
   [use Pinna without Home Assistant](/docs/standalone/).

If something goes wrong, see [Troubleshooting](/docs/troubleshooting/) and
[Recover a device](/docs/recovery/).

## Learn more

- [Goals](/docs/goals/): what Pinna sets out to do, and what it doesn't.
- [Architecture](/docs/architecture/): how the system starts and runs.
- [Contribute](/docs/development/): work on Pinna. The developer pages cover
  the configuration file, `pinnactl` and the control API.
