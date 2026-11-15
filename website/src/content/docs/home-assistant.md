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

## Add the device

1. In Home Assistant, go to **Settings > Devices & services**.
2. Under **Discovered**, find the ESPHome device named after your Pinna device,
   for example `pinna-2a914e`, and select **Add**.

   The name is `pinna-` followed by the last six characters of the device's
   Wi-Fi MAC address, unless you set a name in the
   [configuration](/docs/configure/).

3. If Home Assistant asks for an encryption key, enter the key from
   [Encrypt the connection](#encrypt-the-connection). Otherwise, confirm.

If the device doesn't appear, add it by hand: select **Add integration >
ESPHome**, and enter the device's IP address and port `6053`.

## Choose the Assist pipeline

1. Open the device page in **Settings > Devices & services > ESPHome**.
2. Under **Configuration**, select the **Assistant** that the device uses.
3. Say "Alexa" and ask something, for example "What time is it?"

The light ring turns blue while the device listens, violet while Home
Assistant works on the request, and cyan while the reply plays. See
[Light ring and buttons](/docs/ring-and-buttons/).

## Encrypt the connection

Without a key, the connection between Home Assistant and the device isn't
encrypted. With a key, it uses the same encryption as ESPHome devices
(Noise). Use a key unless your network is completely trusted.

1. Create a key: 32 random bytes, encoded as base64.

   ```sh
   openssl rand -base64 32
   ```

2. Give the key to the device. Use one of these options:

   - Before you build: save the key to `secrets/device/esphome-key`. The image
     then carries it as the device's initial key.
   - On a running device: save the key to `/var/lib/pinna/esphome-key`, make
     the file readable only by root, and restart Pinna:

     ```sh
     chmod 600 /var/lib/pinna/esphome-key
     systemctl restart pinnad
     ```

3. Add the device in Home Assistant, and enter the same key when asked.

Once the connection is encrypted, Home Assistant can replace the key. Pinna
stores the new key in `/var/lib/pinna/esphome-key` and uses it for the next
connection.

If the key file contains something that isn't a valid key, Pinna doesn't
start. Check the log with `journalctl -u pinnad`.

## Check the connection

On the device, run:

```sh
pinnactl status
```

The `home assistant` line shows `connected` and `voice subscribed` when Home
Assistant is connected and the device has an Assist pipeline. The
`wake word` line shows `armed` when the device listens for the wake word.

## What's next

- See every entity that the device adds, in
  [Home Assistant entities](/docs/entities/).
- Learn what the light ring and the buttons do, in
  [Light ring and buttons](/docs/ring-and-buttons/).
- If the device doesn't appear or doesn't respond, see
  [Troubleshooting](/docs/troubleshooting/).
