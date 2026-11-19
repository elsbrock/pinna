---
title: Set up the satellite
description: Open the satellite's dashboard in a browser, set up a new satellite, and find your way around the dashboard's pages.
---

The dashboard is a web page that the device serves on your network. It is
where you set up and control the satellite: its name, Wi-Fi, the wake words,
who answers requests, your services in standalone mode, music sources and
updates. You need no app and no account, and you don't need Home Assistant. It
works without an internet connection: the page and its fonts come from the
device itself.

If you use Home Assistant, it shows many of the same settings, and a change in
one shows in the other. A few things belong to Home Assistant:

- In Home Assistant mode, while Home Assistant is connected, it chooses the
  wake words.
- Only Home Assistant raises a [notification](/docs/ring-and-buttons/#notifications);
  the dashboard clears it.
- Only Home Assistant plays announcements.

## Open the dashboard

Open one of these addresses in a browser on the same network as the device:

- `http://pinna-2a914e.local/`, with the device's name on the network: until
  you [change it](/docs/settings/#change-the-hostname), `pinna-` and the last six characters
  of its MAC address. Your router lists
  it, Home Assistant shows it when it discovers the device, and the dashboard
  shows it under **System** as **Node name**. This works if your computer
  resolves `.local` names; macOS, iOS, Windows 10 and later, and most Linux
  desktops do. On Android, use the IP address if the name doesn't work.
- `http://<address>/`, with the device's IP address from your router.
- In Home Assistant, open the device under **Settings > Devices & services >
  ESPHome** and select **Visit**.
- Over the USB cable: `http://10.42.0.1/`. This works when the Wi-Fi doesn't.
- Over its setup hotspot, while the device has no saved Wi-Fi network:
  `http://10.42.1.1/`. See [Connect to Wi-Fi](/docs/flash/#connect-to-wi-fi).

Each device starts with its own name on the network. If you change it, give
each device a different one.

## Set up a new satellite

1. [Open the dashboard](#open-the-dashboard). Over the USB cable or the setup
   hotspot, first [connect the device to Wi-Fi](/docs/flash/#connect-to-wi-fi)
   on **Network**.
2. [Name the device](/docs/settings/#name-the-device) on **System**, for example `Kitchen`.
   Do this before you add it to Home Assistant, which keeps the first name in
   its entity IDs.
3. [Set the time zone](/docs/settings/#set-the-time-zone) on **System**, so that the
   satellite tells your time in standalone mode, and its own clock and log
   use your zone.
4. Choose who answers requests, on **Assistant**:
   [connect the device to Home Assistant](/docs/home-assistant/), or
   [set up standalone mode](/docs/standalone/). Until Home Assistant connects
   or you switch to standalone mode, **Overview** shows how to do either.

## What the dashboard shows

On a computer the sections are in a sidebar, with the device at its foot. The
open section lists its pages under it, such as **Services**, **Conversation**
and **Tools** under **Assistant**, and **Logs** under **System**. On a phone the
sections are in a bar at the bottom, and a section's pages are rows on it.

| Section | What you can do |
| --- | --- |
| **Overview** | See the device with its light ring lit as it is right now, what it is doing, and its connection to Home Assistant (in standalone mode, whether its services are ready), Wi-Fi, version and uptime at a glance. Set the volume, turn the microphones and the wake word on or off, start or stop a request, clear a waiting [notification](/docs/ring-and-buttons/#notifications), and see the timers and how many messages from the [satellite group](/docs/satellite-groups/#pass-messages-between-satellites) wait to be said. **Now playing** shows what plays and where it comes from, with pause, resume and stop, and **Mute music**; with nothing playing, it says how to play something. **Identify** runs the ring's test pattern, to find the device among others |
| **Voice & sound** (**Voice** in the phone's bar) | Turn the wake word on or off, choose the **Wake word** and **Second wake word** while Home Assistant isn't connected or in standalone mode, turn the microphones on or off, set the volume and the [reply volume](/docs/audio/#volume) (turn **Replies follow Volume** off first), and turn the **Wake sound** on or off. **Ring** sets the ring brightness, turns **Listening direction** on or off, [uses the ring as a light](/docs/ring-and-buttons/#use-the-ring-as-a-light), runs the ring's test pattern with **Identify**, and shows what each ring state means. **Media sources** turns [AirPlay](/docs/airplay/) and [Bluetooth](/docs/bluetooth/) on or off, shows whether they run, pairs phones and forgets them, and [plays music from a URL](/docs/music/#play-music-from-a-url). **Wake words** lists the words on offer and [adds your own](/docs/wake-words/#add-your-own-wake-word). **Sensitivity** opens the **Wake word sensitivity** page, where you [tune the wake word](/docs/wake-words/#tune-the-wake-word). Home Assistant shows the same settings, and a change in one shows in the other |
| **Assistant** | Choose who answers requests: Home Assistant or the satellite itself, in [standalone mode](/docs/standalone/). In Home Assistant mode, see whether Home Assistant is connected and has a voice pipeline for the device, and whether the connection is encrypted, and see the device's name and copy the host and port to add the device by hand. **Services**, **Conversation** and **Tools** set up standalone mode: the speech and language services and their keys, the language, place and prompt, a box to type a question, what the brain may do, radio stations and MCP. **Satellite group** puts satellites in a group, so that they [pass messages](/docs/satellite-groups/#pass-messages-between-satellites) to each other and [control each other](/docs/satellite-groups/#control-another-satellite): turn it on, add and remove members, and turn **Group messages** and **Control from the group** on or off |
| **Network** | See the Wi-Fi network, the signal strength and the addresses. [Change the hostname](/docs/settings/#change-the-hostname). [Change the Wi-Fi network](/docs/settings/#change-the-wi-fi-network) and forget saved ones. If the Wi-Fi doesn't work, the page says which of the [troubleshooting](/docs/troubleshooting/#the-device-has-no-wi-fi) cases applies. [Set a dashboard address](/docs/settings/#set-a-dashboard-address), where else the dashboard opens |
| **System** | [Name the device](/docs/settings/#name-the-device), [set its time zone](/docs/settings/#set-the-time-zone), [reset it](/docs/settings/#reset-the-device), choose where it looks for updates (**Updates**: Stable, Beta or Off), check for one and see what the last check found, see which version runs from which slot, install an update and follow its steps, switch to the version in the other slot, restart the device, see how its last start ended, and see its details. See [Install an update from the dashboard](/docs/update/#install-an-update-from-the-dashboard). **Logs**, under System, shows the log of `pinnad`, and of the AirPlay and Bluetooth services, as it is written. Debug lines are left out, because they can contain what was said |

## Switch between satellites

With more than one device, you can move from one dashboard to the next. On a
computer, select the device at the foot of the sidebar; on a phone, select the
name at the top. A list opens with this device first, then the other devices
on the same subnet, each with its name and the address it opens. Devices on
another subnet or VLAN aren't listed, even if your router passes mDNS between
them. Select one
to open its dashboard in the same tab.

Each device looks for the others once a minute, so the list opens at once. A
device you just turned on can take a minute to show up. One that you turn off,
or whose dashboard stops answering, leaves the list within two minutes.

The link goes to the other device's dashboard address if it has one. If not, it
goes to its `.local` name when you opened this dashboard by a `.local` name, and
to its IP address otherwise (an IPv6 address if you opened this one by IPv6),
because some browsers, such as many on Android, don't find `.local` names. When
a device has a dashboard address, its `.local` name and IP address are listed
under it, in case the address doesn't open.

A device on an older release shows in the list, but its dashboard has no list
to go back with: open it from the address bar, or update it. A device that
isn't on a network yet, only on USB, shows the list without others.

## Who can use the dashboard

The dashboard has no login. **Anyone on your network can use it.** That
includes renaming the device, setting its dashboard address, moving it to
another Wi-Fi network or forgetting the one it uses, making the device play an
address from your network, lighting the ring in any colour, the solid red of
the muted microphones included, restarting the device,
choosing the update channel, installing an update and switching to the version in the other slot. It also
includes seeing what plays: the title of the music a phone sends over AirPlay
or Bluetooth shows on **Now playing**. In
[standalone mode](/docs/standalone/) it includes changing your services and
the prompt, adding MCP servers, and asking questions that you pay for; the
keys themselves never leave the device. Pinna's MCP server, `/mcp`, is open
the same way. In a
[satellite group](/docs/satellite-groups/#pass-messages-between-satellites), it
includes adding any satellite whose **Satellite group** switch is on and
that is in no group, removing members, and turning **Group messages** and **Control from the
group** on or off. Only an update that
is signed with the device's release key installs, and the device goes back to
the previous version if the new one doesn't start. An install from a manifest
URL takes an older signed release too, so someone with a copy of one can
downgrade the device.

The dashboard has no switch to turn it off: on a page without a login, anyone
could turn it back on. Developers can turn it off in the
[developer settings](/docs/configure/#web).

## What's next

- [Connect to Home Assistant](/docs/home-assistant/), or
  [set up standalone mode](/docs/standalone/).
- Change the satellite's name, Wi-Fi network and other
  [settings](/docs/settings/).
- [Update Pinna](/docs/update/) from the dashboard.
