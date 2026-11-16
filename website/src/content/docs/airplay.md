---
title: Play music with AirPlay
description: Turn on the AirPlay receiver, play music from an iPhone, a Mac or Music Assistant, and find out why the device doesn't show up.
---

> **Untested on a device.** AirPlay starts, reports that it is ready and stops
> in the virtual machine, but no AirPlay sender has played to Pinna yet. See
> [Status and roadmap](/docs/status/#known-gaps).

The device can receive AirPlay. iPhones, iPads, Macs and Music Assistant can
then play music on it. The music goes through the device's own speaker and
audio processing, so the wake word keeps working while it plays, and replies,
announcements and timer alarms play over it.

AirPlay is off until you turn it on.

## Before you begin

- The iPhone, the Mac or Music Assistant finds the device by mDNS, so it has to
  be on the same network as the device. Unlike Home Assistant, an iPhone can't
  add the device by its address.
- If they are on different networks or VLANs, your router has to pass mDNS
  between them, and its firewall has to allow both directions: the sender to
  the device on TCP port 5000 and UDP ports 6001 to 6010, and the device back
  to the sender on UDP, to ports that the sender chooses.

## Turn AirPlay on

1. Open the [dashboard](/docs/dashboard/), go to **Voice & sound** (**Voice**
   on a phone) and select **Media sources**, under **Sound**.

   If there is no **Media sources**, the device runs a Pinna from before
   AirPlay. Build a new image and [update the device](/docs/update/).
2. Turn on **AirPlay**. After a few seconds the page shows **Ready as** and the
   name to choose on your iPhone or Mac.

Home Assistant has the same switch: **AirPlay**, under **Configuration** on the
device's page. A change in one shows in the other.

The receiver uses the device's name, the one you set in
[Name the device](/docs/dashboard/#name-the-device). When you rename the
device, AirPlay restarts under the new name, which stops any music that plays.

## Play music

- On an iPhone or an iPad, open the music controls, tap the AirPlay icon and
  choose the device.
- On a Mac, choose the device as the sound output, or in the AirPlay menu of
  the app that plays.
- In Music Assistant, the device is an AirPlay player.

While AirPlay plays:

- The music replaces whatever the device played before: media from Home
  Assistant, or another AirPlay sender. Media that Home Assistant starts
  replaces the music the same way.
- The sender's volume sets how loud the music is. **Volume**, on the dashboard,
  the device's buttons or the **Volume** entity in Home Assistant, sets how loud
  the device is overall.
- The Home Assistant media player's volume, and **Music volume** under
  **Now playing** on the dashboard's **Overview**, is the music's volume, and
  changing it changes the music. The sender's slider doesn't move. When you next move
  the slider, or pause and play on the sender, the sender's volume applies
  again.
- **Stop** or **Pause** in Home Assistant stops the music on the device, but
  not on the sender: the iPhone still shows it playing. **Play** in Home
  Assistant doesn't bring it back. Pause and play on the sender, skip a track
  or seek.

## Limits

- **The device plays on its own.** Pinna has AirPlay 1, not AirPlay 2, so the
  device can't join a group with HomePods or other AirPlay 2 speakers.
- **There is no password.** While AirPlay is on, anyone on your network can
  play music on the device, and replace what it plays.

## If the device doesn't show up

Look at **Voice & sound > Media sources** on the dashboard:

- **Ready as "…"**: the receiver runs. Check that the sender is on the same
  network as the device. A guest Wi-Fi, a separate VLAN or client isolation on
  the Wi-Fi hides the device, unless your router passes mDNS and the ports in
  [Before you begin](#before-you-begin).
- **Not running**, with "AirPlay didn't start": the receiver doesn't run. The
  log under **System > Logs** says that it stopped, followed by the
  receiver's own lines (`pinna-airplay`, `shairport-sync`).
- **The AirPlay switch is greyed out, and the page says the device has no
  working speaker**: **System > Logs** says why the speaker didn't start. The
  switch is also greyed out while the dashboard has lost the connection to the
  device.

## What's next

- [Audio](/docs/audio/): how the device plays and mixes music, replies and
  alarms.
- [Home Assistant entities](/docs/entities/): the **AirPlay** switch and the
  media player.
