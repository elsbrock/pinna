---
title: Connect to Home Assistant
description: Add a Pinna device to Home Assistant, encrypt the connection, and choose the Assist pipeline it uses.
---

In Home Assistant mode, Home Assistant answers the satellite's requests with
your Assist pipeline. Pinna talks to Home Assistant over the ESPHome native
API, the same protocol that ESPHome devices use. Home Assistant finds the
device on your network, and you add it like any other ESPHome device. You
don't need YAML or an add-on.

## Before you begin

- The device runs Pinna and is connected to Wi-Fi. See
  [Install Pinna](/docs/flash/).
- You gave the device a name on its dashboard, for example `Kitchen`. See
  [Name the device](/docs/settings/#name-the-device). Home Assistant builds
  the entity IDs from the name the device has when you add it.
- Home Assistant runs on the same network as the device, so that it can find
  the device by mDNS.
- You have an Assist pipeline with speech-to-text and text-to-speech. To set
  one up, see
  [Assist](https://www.home-assistant.io/voice_control/) in the Home Assistant
  documentation.
- The satellite is in Home Assistant mode, as a new one is. If you switched
  it to standalone mode, open **Assistant** on the dashboard, select **Home
  Assistant** under **Mode**, and then select **Switch**. You can also switch
  by voice, in both directions: see [Switch by
  voice](/docs/standalone/#switch-by-voice).

## Encryption

Home Assistant encrypts the connection itself. When it adds the device, it
gives the device a key, the same encryption that ESPHome devices use, and from
then on the device accepts nothing else. A device that Home Assistant added
unencrypted gets its key the next time Home Assistant connects. You never see
or type the key. The **Assistant** page of the
[dashboard](/docs/dashboard/) shows whether the connection is encrypted.

This needs a Home Assistant whose ESPHome integration sets keys on devices. An
older one keeps the connection unencrypted, and the dashboard says so.

Until it has a key, the device takes the first key that anyone on your network
sends it. If someone else sets one first, Home Assistant can't connect.
[Reset the device](/docs/settings/#reset-the-device), which removes the key,
and add it again. With a dashboard password set, the device takes a key only
while you let Home Assistant add it; see
[Add a device that has a dashboard password](#add-a-device-that-has-a-dashboard-password).

A developer can build the key into the image instead; then Home Assistant
asks for it when you add the device. See
[A fixed encryption key](/docs/configure/#a-fixed-encryption-key).

## Add the device

1. In Home Assistant, go to **Settings > Devices & services**.
2. Under **Discovered**, find the ESPHome device and select **Add**. It
   appears under its name: the one you gave it on the
   [dashboard](/docs/settings/#name-the-device), or the model and the end of
   its MAC address, for example `Echo Dot 2a914e`. Name it before you add it:
   Home Assistant keeps the first name in the entity IDs.
3. Home Assistant sets the encryption key itself; see
   [Encryption](#encryption). It asks you for a key only when the image came
   with one, or when the device still has a key that an earlier Home
   Assistant set. For an image's key, enter it. Otherwise
   [reset the device](/docs/settings/#reset-the-device), which removes the
   old key, and add it again.

If the device doesn't appear, add it by hand: open the **Assistant** page
of the [dashboard](/docs/dashboard/), copy the host and the port under **Add it
to Home Assistant**, and enter them in Home Assistant under **Add integration >
ESPHome**.

## Add a device that has a dashboard password

With a dashboard password set and no key from Home Assistant yet, the device
refuses every connection to Home Assistant. Home Assistant may still list the
device, but it can't connect to it. You open a window first:

1. Open the dashboard of the device and sign in. In Home Assistant mode,
   select **Home Assistant**. In standalone mode, select **Assistant**.
2. Select **Let Home Assistant add this satellite**. The page shows how much
   of the 15 minutes is left.
3. In Home Assistant, add the device as in [Add the device](#add-the-device).
   If the 15 minutes pass first, select the button again.

Inside the window, the device takes the first key that anyone on your network
sends it, as it does without a password, so add it right away. Once Home
Assistant has set its key, only that key works, and the window no longer
matters. Setting a password while Home Assistant is connected without a key
ends that connection: open the window and add the device again. A device that
already has a key keeps it when you set a password, and needs no window.

## Choose the Assist pipeline

1. Open the device page in **Settings > Devices & services > ESPHome**.
2. Under **Configuration**, select the **Assistant** that the device uses.
3. Say "Alexa" and ask something, for example "What time is it?"

The light ring turns blue while the device listens, violet while Home
Assistant works on the request, and cyan while the reply plays. See
[Light ring and buttons](/docs/ring-and-buttons/).

The device plays the reply while it downloads. If both the conversation agent
and the text-to-speech of the pipeline support streaming, Home Assistant
starts a long reply while the agent still writes it, and you hear it sooner.
A short reply gains little. If the agent is slow, a streamed reply can pause
in the middle. See
[The reply starts late or pauses](/docs/troubleshooting/#the-reply-starts-late-or-pauses).

## Choose the wake words

While Home Assistant is connected, it chooses the wake words, on the device
page under **Configuration**. See
[Choose the wake words](/docs/wake-words/#in-home-assistant).

## Change the key later

Home Assistant can replace the key over the encrypted connection. Pinna stores
the new key and uses it for the next connection.

A [factory reset](/docs/settings/#reset-the-device) or a reinstall from
fastboot removes the key. After either, remove the device from Home Assistant
and add it again, and Home Assistant gives it a new key.

## Check the connection

Open the **Assistant** page of the [dashboard](/docs/dashboard/).
**Home Assistant** shows **Connected** when Home Assistant is connected, and
**Voice pipeline** shows **Ready** when it has an Assist pipeline for the
device. **Encryption** shows whether the connection is encrypted.

## What's next

- To have the device answer requests itself, with the services you choose, see
  [Set up standalone mode](/docs/standalone/). Home Assistant can stay
  connected in standalone mode, without the Assist satellite.
- See every entity that the device adds, in
  [Home Assistant entities](/docs/entities/).
- Learn what the light ring and the buttons do, in
  [Light ring and buttons](/docs/ring-and-buttons/).
- If the device doesn't appear or doesn't respond, see
  [Troubleshooting](/docs/troubleshooting/).
