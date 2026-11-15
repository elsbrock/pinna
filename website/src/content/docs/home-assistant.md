---
title: Connect to Home Assistant
description: Add a Pinna device to Home Assistant, encrypt the connection, and choose the Assist pipeline it uses.
---

Pinna talks to Home Assistant over the ESPHome native API, the same protocol
that ESPHome devices use. Home Assistant finds the device on your network, and
you add it like any other ESPHome device. You don't need YAML or an add-on.

## Before you begin

- The device runs Pinna and is connected to Wi-Fi. See
  [Install Pinna](/docs/flash/).
- Home Assistant runs on the same network as the device, so that it can find
  the device by mDNS.
- You have an Assist pipeline with speech-to-text and text-to-speech. To set
  one up, see
  [Assist](https://www.home-assistant.io/voice_control/) in the Home Assistant
  documentation.

## Set an encryption key (optional)

Without a key, the connection between Home Assistant and the device isn't
encrypted. With a key, it uses the same encryption as ESPHome devices. Use a
key unless you trust everything on your network. Set the key before you add
the device, so that the connection is never unencrypted.

1. Create a key: 32 random bytes, encoded as base64.

   ```sh
   openssl rand -base64 32
   ```

2. Give the key to the device. Use one of these options:

   - Before you build: save the key to `secrets/device/esphome-key`. The image
     then carries it as the device's first key.
   - On a running device: save the key to `/var/lib/pinna/esphome-key`, make
     the file readable only by `root`, and restart Pinna:

     ```sh
     chmod 600 /var/lib/pinna/esphome-key
     systemctl restart pinnad
     ```

3. Keep the key at hand. Home Assistant asks for it when you add the device.

If the key file contains something that isn't a valid key, Pinna doesn't
start. Check the log with `journalctl -u pinnad`.

If you add a key to a device that Home Assistant already uses without one,
Home Assistant can't connect anymore and asks you for the key.

## Add the device

1. In Home Assistant, go to **Settings > Devices & services**.
2. Under **Discovered**, find the ESPHome device. It appears under its
   friendly name, `Echo Dot` unless you changed it in the
   [configuration](/docs/configure/), and select **Add**.
3. If you set a key, enter it when Home Assistant asks for it.

If the device doesn't appear, add it by hand:

1. Find the device's address. On the device, run:

   ```sh
   ip -4 addr show wlan0
   ```

2. In Home Assistant, select **Add integration > ESPHome**, and enter the
   address and port `6053`.

## Choose the Assist pipeline

1. Open the device page in **Settings > Devices & services > ESPHome**.
2. Under **Configuration**, select the **Assistant** that the device uses.
3. Say "Alexa" and ask something, for example "What time is it?"

The light ring turns blue while the device listens, violet while Home
Assistant works on the request, and cyan while the reply plays. See
[Light ring and buttons](/docs/ring-and-buttons/).

## Change the key later

Once the connection is encrypted, Home Assistant can replace the key. Pinna
stores the new key in `/var/lib/pinna/esphome-key` and uses it for the next
connection.

Reinstalling Pinna from fastboot erases `/var`, and with it the key. After a
reinstall, give the device its key again, as in
[Set an encryption key](#set-an-encryption-key-optional), or remove the device
from Home Assistant and add it again.

## Check the connection

On the device, run:

```sh
pinnactl status
```

The `home assistant` line shows `connected, voice subscribed` when Home
Assistant is connected and has an Assist pipeline for the device. The
`wake word` line shows `armed` when the device listens for the wake word.

## What's next

- See every entity that the device adds, in
  [Home Assistant entities](/docs/entities/).
- Learn what the light ring and the buttons do, in
  [Light ring and buttons](/docs/ring-and-buttons/).
- If the device doesn't appear or doesn't respond, see
  [Troubleshooting](/docs/troubleshooting/).
