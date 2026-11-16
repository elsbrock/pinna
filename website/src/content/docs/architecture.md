---
title: Architecture
description: How Pinna starts, how its storage is laid out, which services run, and how a voice request flows through the system.
---

This page explains how Pinna is put together, from the bootloader to the
voice request. The reasons behind each decision are in the decision records in
[`docs/adr/`](https://github.com/elsbrock/pinna/tree/main/docs/adr).

## Hardware

The Echo (2nd gen) and the Echo Dot (2nd gen) share the MT8163 platform: a
MediaTek MT8163 processor, which runs Pinna in 32-bit ARM mode, and about
480 MB of memory. The parts that Pinna uses:

- Seven microphones on top, and a speaker.
- A ring of 12 RGB LEDs behind a white diffuser, driven by an IS31FL3236 LED
  controller.
- Four buttons, and a hardware switch that cuts the microphones.
- Wi-Fi in the MT8163.
- USB, which Pinna uses for a serial console and a network connection to your
  computer: a micro-USB port on the Echo Dot (2nd gen), and the pins in the
  base on the Echo (2nd gen).

## Boot

1. The bootloader always loads the boot image from `boot_a`. It contains the
   Linux kernel (Debian's 6.12, with the MT8163 board support ported to it),
   the device tree, and a small initramfs.
2. The kernel ignores the command line from the bootloader and uses its own.
3. `pinna-init`, the program in the initramfs, reads the slot state from the
   `misc` partition and chooses a root filesystem slot: `system_a` or
   `system_b`. See [Updates](#updates).
4. `pinna-init` checks the slot's signature, sets up dm-verity, and mounts the
   root filesystem read-only.
5. systemd starts. On the first boot, it formats the `userdata` partition as
   `/var`. Then it starts Pinna's services.
6. About 10 seconds after the services start, `pinna-boot-ok` checks that `pinnad`
   answers and SSH runs. If they do, it marks the slot as good and cancels the
   fastboot request. If they don't within five minutes, it restarts the device.

## Storage

| Partition | Size | Contents |
| --- | --- | --- |
| `boot_a` | 16 MiB | Boot image: kernel, device tree, initramfs with `pinna-init` |
| `boot_b` | 16 MiB | A copy of the boot image, as a manual fallback |
| `system_a`, `system_b` | 768 MiB each | Two root filesystem slots |
| `userdata` | The rest | `/var`: settings, the Wi-Fi configuration, keys, logs |
| `misc` | 512 KiB | The bootloader command (the fastboot request) and Pinna's slot state |

A root filesystem slot holds three parts:

1. The root filesystem, as a read-only erofs image.
2. A 4 KiB footer with the image version, the dm-verity parameters and an
   Ed25519 signature.
3. The dm-verity hash tree. The kernel checks every block it reads against
   this tree, so a changed or damaged block can't be read unnoticed.

The root filesystem never changes on the device. Everything that changes is in
`/var`: Pinna's settings in `/var/lib/pinna`, SSH host keys, and the systemd
journal.

## Updates

An update writes a new image into the slot that isn't running, and marks it
for a trial boot with three attempts. `pinna-init` takes one attempt each
time it boots the trial slot. When the health check passes, the slot becomes
good. When the attempts run out, `pinna-init` returns to the other slot. For
the steps, see [Update Pinna](/docs/update/).

The boot image has only one active copy, in `boot_a`, so updates don't change
it. You install a new boot image from fastboot.

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
| `systemd-resolved` | Name resolution and mDNS, which Home Assistant and AirPlay senders use to find the device |
| `ssh` | Remote login with keys |
| `systemd-journald` | Logs, kept in `/var/log/journal` |
| `pinna-pipewire`, `pinna-wireplumber` | The microphone processing graph. See [Audio](/docs/audio/) |

Pinna's own services:

| Service | Job |
| --- | --- |
| `pinnad` | The voice satellite. Talks to Home Assistant, detects the wake word, drives the ring and reads the buttons. Serves the dashboard and the control API on port 80, and the control API on `127.0.0.1:7470` for `pinnactl` and the health check |
| `pinna-wifi` | Powers up the Wi-Fi chip and loads its firmware |
| `pinna-gadget` | Sets up the USB serial console and network |
| `pinna-bootstages` | Shows the start-up steps on the ring after the system starts (USB, Wi-Fi, `pinnad`), then hands the ring to `pinnad` |
| `pinna-fastboot-guard` | Writes the fastboot request early in each boot |
| `pinna-boot-ok` | The health check that marks a boot as good |
| `pinna-airplay` | The AirPlay receiver: runs Debian's `shairport-sync` and passes its audio to `pinnad`. `pinnad` starts and stops it with the **AirPlay** switch. See [Play music with AirPlay](/docs/airplay/) |
| `pinna-bluetooth.target` | Bluetooth: Debian's `bluetoothd` and BlueALSA, and `pinna-bt`, which passes a phone's audio to `pinnad`. `pinnad` starts and stops them with the **Bluetooth** switch. See [Play music over Bluetooth](/docs/bluetooth/) |

## A voice request

1. `pinnad` reads audio from the microphones in 20 ms frames and runs the wake
   word detection on it.
2. When it hears the wake word, it asks Home Assistant to start an Assist run,
   and streams the audio, starting 200 milliseconds before the end of the wake
   word.
3. Home Assistant detects the end of speech, turns the speech into text,
   handles the request, and sends a link to the spoken reply. With a pipeline
   that streams, the link comes while the reply is still being written.
4. `pinnad` plays the reply on the speaker while it downloads. When you say
   the wake word during the reply, the reply stops and a new request starts.
5. If Home Assistant asks for a follow-up, the device listens again without
   the wake word.

At each step, `pinnad` sets the light ring and sends a `voice_assistant` event
to Home Assistant.

## What's next

- [Audio](/docs/audio/): how the microphones and the speaker are processed.
- [Repository layout](/docs/crates/): where each part lives in the code.
