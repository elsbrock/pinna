---
title: Play music over Bluetooth
description: Turn on Bluetooth, pair a phone, play music from it, and find out why it doesn't connect.
---

The device can be a Bluetooth speaker for phones and tablets. The music goes
through the device's own speaker and audio processing, so the wake word keeps
working while it plays, and replies, announcements and timer alarms play over
it.

Bluetooth is off until you turn it on. Only phones that you pair while the
pairing window is open can connect.

> **Not yet tested with Pinna on the Echo Dot (2nd gen).** Bluetooth was
> tested with an iPhone on the Echo (2nd gen). See
> [Status and roadmap](/docs/status/#known-gaps).

## Before you begin

- Use a 5 GHz Wi-Fi if you can. The device's Wi-Fi and Bluetooth share one
  antenna. On a 2.4 GHz Wi-Fi, while music plays over Bluetooth, replies start
  slightly later, and large downloads, such as an update, slow down a lot or
  stall. The **Network** page of the [dashboard](/docs/dashboard/) shows the
  band. On 5 GHz, Bluetooth doesn't affect the Wi-Fi.
- Calls don't work: the device plays music, but isn't a headset or a
  speakerphone.

## Turn Bluetooth on

1. Open the [dashboard](/docs/dashboard/), go to **Voice & sound** (**Voice**
   on a phone) and select **Media sources**, under **Sound**.

   If there is no **Media sources**, or no **Bluetooth** on it, the device runs
   a Pinna from before Bluetooth. Build a new image,
   [update the device](/docs/update/), and also install the new boot image, as
   [Update the boot image](/docs/update/#update-the-boot-image) describes:
   with an older one, phones see every Echo under the same address.
2. Turn on **Bluetooth**. After a few seconds the page shows **Ready as** and
   the name that phones list it under.

Home Assistant has the same switch: **Bluetooth audio**, under **Configuration** on
the device's page. A change in one shows in the other.

The device uses its name, the one you set in
[Name the device](/docs/dashboard/#name-the-device).

## Pair a phone

1. On **Media sources**, select **Pair a phone**. In Home Assistant, turn on
   **Bluetooth pairing** on the device's page instead. It turns itself off
   when the window closes.

   For two minutes, phones nearby can find the device, and two blue lights turn
   slowly around the ring.
2. On the phone, open the Bluetooth settings and choose the device's name. The
   phone pairs without a code.

The pairing window closes when a phone pairs, after two minutes, or when you
select **Stop pairing**. The paired phone shows on **Media sources**, and connects
again later without pairing.

While the window is open, anyone nearby can pair a phone. Outside it, the
device is invisible to phones that haven't paired.

## Play music

On the phone, choose the device as the audio output, in the Bluetooth
settings or in Control Center, and play.

While Bluetooth plays:

- The music replaces whatever the device played before: media from Home
  Assistant, AirPlay, or another phone. Media that Home Assistant starts
  replaces the music the same way.
- When two paired phones are connected, the device plays only the one that
  connected last, even while it plays nothing. If the music stops when
  another phone connects, disconnect that phone.
- The phone's volume sets how loud the music is. **Volume**, on the dashboard,
  the device's buttons or the **Volume** entity in Home Assistant, sets how loud
  the device is overall.
- The Home Assistant media player's volume, and **Music volume** under
  **Now playing** on the dashboard's **Overview**, is the music's volume, and
  changing it changes the music. The phone's slider doesn't move. When you next move
  the slider, or pause and play on the phone, the phone's volume applies
  again.
- **Stop** or **Pause** in Home Assistant stops the music on the device, but
  not on the phone. **Play** in Home Assistant doesn't bring it back. Pause on
  the phone, wait a few seconds, then play.

Turning Bluetooth off disconnects the phones. They stay paired: to play again
after turning it on, choose the device in the phone's Bluetooth settings,
without pairing.

## Forget a phone

With **Bluetooth** on, select **Forget** next to the phone on **Media
sources**. It can't connect any more. To pair it again, also remove the device
from the phone's Bluetooth settings, then [pair it](#pair-a-phone) again.

To add another phone, you don't need to forget the first one: pair the new
one, and both stay paired.

## If a phone doesn't find the device or doesn't connect

Look at **Voice & sound > Media sources** on the dashboard:

- **Ready as "…"**: Bluetooth runs. A phone that hasn't paired finds the
  device only while the pairing window is open: select **Pair a phone** first.
  A phone that was forgotten on the device has to forget the device too, then
  pair again.
- **Ready to pair as "…"**: the pairing window is open, and the page shows
  how long it has left. Keep the phone's Bluetooth settings open: the device
  can take a few seconds to show up. If the window ran out, select **Pair a
  phone** again.
- **Not running**, with "Bluetooth didn't come up last time": the device
  restarted before Bluetooth was running with the Wi-Fi up, either while it
  was still starting or while the Wi-Fi was down. So that Bluetooth can't
  keep the device off the network, it stays off after a restart like that.
  Select **Try again**.
- **Not running**, with another reason, or **Starting…** for more than about
  30 seconds: Bluetooth didn't start. **System > Logs** shows the log of
  Bluetooth's services (`bluetoothd`, `bluealsa` and `pinna-bt`) next to
  `pinnad`'s. Select **Try again** once the cause is fixed. While the window
  is open, `pinna-bt` logs `pairing window open`, and each refused pairing.
- **The Bluetooth switch is greyed out, and the page says the device has no
  Bluetooth or no working speaker**: **System > Logs** says why the speaker
  didn't start. The virtual Echo Dot has no Bluetooth. The switch is also
  greyed out while the dashboard has lost the connection to the device.

## What's next

- [Audio](/docs/audio/): how the device plays and mixes music, replies and
  alarms.
- [Home Assistant entities](/docs/entities/): the **Bluetooth audio** and
  **Bluetooth pairing** switches and the media player.
