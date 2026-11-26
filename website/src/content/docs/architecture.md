---
title: Architecture
description: How Pinna starts, how its storage is laid out, which services run, and how a voice request flows through the system.
---

This page explains how Pinna is put together, from the bootloader to the
voice request.

## Overview

![Pinna's architecture: what connects to the satellite, what the satellite connects to, and the layers that run on it.](/diagrams/overview.svg "Each arrow starts at the side that opens the connection. Select pinnad, PipeWire or a layer to read more about it.")

## Hardware

The Echo (2nd gen) and the Echo Dot (2nd gen) share the MT8163 platform: a
MediaTek MT8163 processor, which runs Pinna in 32-bit ARM mode, and about
480 MB of memory. The parts that Pinna uses on them:

- Seven microphones on top, and a speaker.
- A ring of 12 RGB LEDs behind a white diffuser, driven by an IS31FL3236 LED
  controller.
- Four buttons, and a hardware switch that cuts the microphones.
- Wi-Fi in the MT8163.
- USB, which Pinna uses for a serial console and a network connection to your
  computer: a micro-USB port on the Echo Dot (2nd gen), and the pins in the
  base on the Echo (2nd gen).

The Echo Dot (5th gen) has a different platform: a MediaTek MT8519 processor,
which also runs Pinna in 32-bit ARM mode, and 512 MB of memory. It has three
microphones, a ring of 12 RGB LEDs under the edge of its sphere, three buttons
and a hardware switch that cuts the microphones, a speaker amplifier and
Wi-Fi on a MediaTek MT7663 chip. Its system is on NAND flash, in UBI volumes
instead of eMMC partitions, and it has no USB connector. Its bootloader is
Amazon's U-Boot, which loads a boot image in a different format and has no
fastboot mode. The rest of this page describes the Echo (2nd gen) and the Echo
Dot (2nd gen), the two that install over fastboot; where the Echo Dot (5th
gen) differs, the section says so.

## Boot

1. The bootloader's first stage, the preloader, reads the slot record in the
   `misc` partition: which slot starts, and how many attempts a slot on trial
   has left. It counts the boot if the slot is on trial, and the bootloader
   loads that slot's boot image: `boot_a` or `boot_b`. It contains the Linux kernel
   (Debian's 6.12, with the MT8163 board support ported to it), the device
   tree, and a small initramfs.
2. The kernel ignores the command line from the bootloader and uses its own.
3. `pinna-init`, the program in the initramfs, reads which slot the
   bootloader started from the device tree, and takes the root filesystem of
   the same slot: `system_a` or `system_b`. See [Updates](#updates).
4. `pinna-init` checks the slot's signature, sets up dm-verity, and mounts the
   root filesystem read-only. If that fails and the other slot is marked
   good, it marks this slot unbootable and restarts into the other one.
5. systemd starts. On the first boot, it formats the `userdata` partition as
   `/var`. Then it starts Pinna's services.
6. About 10 seconds after the services start, `pinna-boot-ok` checks that `pinnad`
   answers and SSH runs. If they do, it marks the slot as good and cancels the
   fastboot request. If they don't within five minutes, it restarts the device.

## Storage

| Partition | Size | Contents |
| --- | --- | --- |
| `boot_a`, `boot_b` | 16 MiB each | The boot image of slot A and of slot B: kernel, device tree, initramfs with `pinna-init` |
| `system_a`, `system_b` | 768 MiB each | The root filesystem of slot A and of slot B |
| `userdata` | The rest | `/var`: settings, the Wi-Fi configuration, keys, logs |
| `misc` | 512 KiB | The bootloader command (the fastboot request) and the bootloader's slot record |

A slot's root filesystem partition holds three parts:

1. The root filesystem, as a read-only erofs image.
2. A 4 KiB footer with the image version, the dm-verity parameters and an
   Ed25519 signature.
3. The dm-verity hash tree. The kernel checks every block it reads against
   this tree, so a changed or damaged block can't be read unnoticed.

The root filesystem never changes on the device. Everything that changes is in
`/var`: Pinna's settings in `/var/lib/pinna`, SSH host keys, and the systemd
journal.

## Updates

An update writes a new boot image and root filesystem into the slot that
isn't running, and marks it in the slot record for a trial boot with three
attempts. The bootloader takes one attempt each time it starts the trial
slot. When the health check passes, Pinna marks the slot good. When the
attempts run out, the bootloader starts the other slot, kernel and root
filesystem together. The running slot stays good the whole time, so one slot
can always start. For the steps, see [Update Pinna](/docs/update/).

Each boot image is marked with its slot (`pinna.boot=a` or `=b`), and the
bootloader passes the mark on, so Pinna sees which boot partition the kernel
came from. Pinna writes `boot_a` in an update only after a healthy boot from
slot B has shown that the bootloader loads `boot_b`. Kaeru builds from before
22 October 2026 always load `boot_a`; there, updates leave the kernel alone and
a new kernel needs fastboot. See
[Update the boot image](/docs/update/#update-the-boot-image).

## The fastboot request

Pinna relies on Kaeru, a payload of the bootloader unlock, which opens fastboot
when the `misc` partition contains the command `bootonce-bootloader`.

Early in each boot, Pinna writes that command, and it removes it once the
health check passes. If the health check fails, Pinna restarts the device into
fastboot. If a boot hangs and you unplug the device, it also opens
fastboot instead of the broken system. During the trial boot of an update,
Pinna doesn't write the command, because the slot fallback already handles a
failure.

## Services

Pinna is a Debian trixie system with systemd. Standard services do the generic
work:

| Service | Job |
| --- | --- |
| `systemd-networkd`, `wpa_supplicant@wlan0` | Wi-Fi, and the USB network to your computer |
| `systemd-resolved` | Name resolution and mDNS, which Home Assistant, AirPlay senders and Music Assistant use to find the device |
| `ssh` | Remote login with keys |
| `systemd-journald` | Logs, kept in `/var/log/journal` |
| `pinna-pipewire`, `pinna-wireplumber` | The microphone processing graph. See [Audio](/docs/audio/) |

Pinna's own services:

| Service | Job |
| --- | --- |
| `pinnad` | The voice satellite. Talks to Home Assistant, detects the wake word, drives the ring and reads the buttons. In standalone mode, runs each request itself with the services you set up. Serves the dashboard, the control API, standalone mode's setup API and its MCP server on port 80, and the control API on `127.0.0.1:7470` for `pinnactl` and the health check. While **Sendspin** is on, it is the Sendspin player too, on port 8928. See [Play music with Sendspin](/docs/sendspin/) |
| `pinna-wifi` | Powers up the Wi-Fi chip and loads its firmware |
| `pinna-gadget` | Sets up the USB serial console and network |
| `pinna-bootstages` | Shows the start-up steps on the ring after the system starts (USB, Wi-Fi, `pinnad`), then hands the ring to `pinnad` |
| `pinna-hostname` | Gives the satellite its name on the network before the network starts |
| `pinna-factory-reset` | After you ask for a factory reset in the dashboard, clears Pinna's settings in `/var` on the next start |
| `pinna-fastboot-guard` | Writes the fastboot request early in each boot |
| `pinna-boot-ok` | The health check that marks a boot as good |
| `pinna-airplay` | The AirPlay receiver: runs Debian's `shairport-sync` and passes its audio to `pinnad`. `pinnad` starts and stops it with the **AirPlay** switch. See [Play music with AirPlay](/docs/airplay/) |
| `pinna-bluetooth.target` | Bluetooth: Debian's `bluetoothd` and BlueALSA, and `pinna-bt`, which passes a phone's audio to `pinnad`. `pinnad` starts and stops them with the **Bluetooth** switch. See [Play music over Bluetooth](/docs/bluetooth/) |

## Inside `pinnad`

`pinnad` runs the voice satellite as one process. One task in it, the state
machine, decides what the satellite does. Every input waits in one channel
and is handled in order, so a button press and a command from Home Assistant
never change the state at the same time. A standalone request runs as a task
of its own and reports each step to the state machine.

![Inside pinnad: the inputs reach the state machine through one channel, and the state machine drives the outputs.](/diagrams/pinnad.svg "Select the wake word, the speaker, the ring or the standalone request to read more about it.")

Audio doesn't pass through the state machine. The capture loop sends the
microphone audio straight to Home Assistant or to the standalone request, and
AirPlay, Bluetooth and Sendspin play straight into the speaker's media lane.
See [Audio](/docs/audio/).

## A voice request

In Home Assistant mode, Home Assistant answers each request:

1. `pinnad` reads audio from the microphones in 20 ms frames and runs the wake
   word detection on it.
2. When it hears the wake word, it asks Home Assistant to start an Assist run,
   and streams the audio, starting 200 milliseconds before the end of the wake
   word.
3. Home Assistant detects the end of speech, turns the speech into text,
   handles the request, and sends a link to the spoken reply. With a pipeline
   that streams, the link comes while the reply is still being written.
4. `pinnad` plays the reply on the speaker while it downloads. When you say
   the wake word while Home Assistant handles the request or during the
   reply, the request and its reply stop and a new request starts.
5. If Home Assistant asks for a follow-up, the device listens again without
   the wake word.

At each step, `pinnad` sets the light ring and sends a `voice_assistant` event
to Home Assistant.

## A request in standalone mode

In [standalone mode](/docs/standalone/), `pinnad` runs the request itself,
with the speech-to-text, brain and text-to-speech services you set up on the
dashboard. Home Assistant takes no part. The request runs as a task of its
own and reports each step to `pinnad`'s state machine, which keeps the phase,
the ring and the speaker.

1. When it hears the wake word, `pinnad` passes the microphone audio to the
   request, starting 200 milliseconds before the end of the wake word.
2. The request detects the end of speech with the wake word detection's
   energy VAD (`pinna-vad`): speech starts after 300 ms of voice and ends
   after 800 ms of silence. If it hears no speech within 5 seconds, the
   request ends and nothing is uploaded. It records at most 15 seconds.
3. The recording goes to the speech-to-text service. A transcript that
   speech-to-text invents on silence is dropped.
4. The brain gets the prompt, the earlier turns of the conversation and the
   transcript, and streams its answer. A model gets the device's tools and
   those of your MCP servers: `pinnad` runs the tool calls it asks for, sends
   the results back and asks again, for at most 5 rounds. An agent, such as
   Hermes or OpenClaw, gets the request and answers it with its own tools.
5. The answer is cut into sentences as it arrives. Text-to-speech speaks each
   sentence, and it plays on the speaker's voice lane while the next ones are
   made, at most two ahead.
6. With follow-ups on, the device listens for 4 seconds more without the wake
   word.

Each stage has a time limit. A request that fails stops, plays the error cue,
and the dashboard's **Assistant** page shows why. The wake word or the Action
button stops a request at any step. The conversation stays in memory only,
for 5 minutes after the last request.

## What's next

- [Audio](/docs/audio/): how the microphones and the speaker are processed.
- [Repository layout](/docs/crates/): where each part lives in the code.
