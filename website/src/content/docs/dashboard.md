---
title: Use the dashboard
description: Open the device's dashboard in a browser to see its state, change settings, install updates, and read its network details and log.
---

The device serves a small web page, the dashboard, on port 80. It shows what
the device is doing and lets you change what Home Assistant can change,
except for these:

- In Home Assistant mode, while Home Assistant is connected, it chooses the
  wake words.
- Only Home Assistant raises a [notification](/docs/ring-and-buttons/#notifications);
  the dashboard clears it.
- Only Home Assistant plays announcements.

It also does what Home Assistant can't:
it installs updates, restarts the device, shows the network details and the
log, and runs the ring's test pattern. It works without an internet
connection: the page and its fonts come from the device itself.

## Open the dashboard

Open one of these addresses in a browser on the same network as the device:

- `http://pinna-2a914e.local/`, with the device's name on the network: until
  you [change it](#change-the-hostname), `pinna-` and the last six characters
  of its MAC address. Your router lists
  it, Home Assistant shows it when it discovers the device, and the dashboard
  shows it under **System** as **Node name**. This works if your computer
  resolves `.local` names; macOS, iOS, Windows 10 and later, and most Linux
  desktops do. On Android, use the IP address if the name doesn't work.
- `http://<address>/`, with the device's IP address from your router.
- In Home Assistant, open the device under **Settings > Devices & services >
  ESPHome** and select **Visit**.
- Over the USB cable: `http://10.42.0.1/`. This works when the Wi-Fi doesn't.

Each device starts with its own name on the network. If you change it, give
each device a different one.

## What the dashboard shows

On a computer the sections are in a sidebar, with the device at its foot; on a
phone they are in a bar at the bottom.

| Section | What you can do |
| --- | --- |
| **Overview** | See the device with its light ring lit as it is right now, what it is doing, and its connection to Home Assistant (in standalone mode, whether its services are ready), Wi-Fi, version and uptime at a glance. Set the volume, turn the microphones and the wake word on or off, start or stop a request, clear a waiting [notification](/docs/ring-and-buttons/#notifications), and see the timers. **Now playing** shows what plays and where it comes from, with pause, resume and stop, and **Mute music**; with nothing playing, it says how to play something. **Identify** runs the ring's test pattern, to find the device among others |
| **Voice & sound** (**Voice** in the phone's bar) | Turn the wake word on or off, choose the **Wake word** and **Second wake word** while Home Assistant isn't connected, turn the microphones on or off, set the volume and the [reply volume](/docs/audio/#volume) (turn **Replies follow Volume** off first), set the ring brightness, turn the **Wake sound** and **Listening direction** on or off, [use the ring as a light](/docs/ring-and-buttons/#use-the-ring-as-a-light), and run the ring's test pattern with **Identify**. **Media sources** turns [AirPlay](/docs/airplay/) and [Bluetooth](/docs/bluetooth/) on or off, shows whether they run, pairs phones and forgets them, and [plays music from a URL](#play-music-from-a-url). **Sensitivity** opens the **Wake word** page, where you [tune the wake word](#tune-the-wake-word). Home Assistant shows the same settings, and a change in one shows in the other. The page also shows what each ring state means |
| **Assistant** | Choose who answers requests: Home Assistant or the satellite itself, in [standalone mode](/docs/standalone/). In Home Assistant mode, see whether Home Assistant is connected and has a voice pipeline for the device, and whether the connection is encrypted, and see the device's name and copy the host and port to add the device by hand. **Services**, **Conversation** and **Tools** set up standalone mode: the speech and language services and their keys, the language, place and prompt, a box to type a question, what the brain may do, radio stations and MCP |
| **Network** | See the Wi-Fi network, the signal strength and the addresses. [Change the hostname](#change-the-hostname). [Change the Wi-Fi network](#change-the-wi-fi-network) and forget saved ones. If the Wi-Fi doesn't work, the page says which of the [troubleshooting](/docs/troubleshooting/#the-device-has-no-wi-fi) cases applies. [Set a dashboard address](#set-a-dashboard-address), where else the dashboard opens |
| **System** | [Name the device](#name-the-device), [set its time zone](#set-the-time-zone), [reset it](#reset-the-device), choose where it looks for updates (**Updates**: Stable, Beta or Off), check for one and see what the last check found, see which version runs from which slot, install an update and follow its steps, switch to the version in the other slot, restart the device, see how its last start ended, and see its details. See [Install an update from the dashboard](/docs/update/#install-an-update-from-the-dashboard). **Logs**, under System, shows the log of `pinnad`, and of the AirPlay and Bluetooth services, as it is written. Debug lines are left out, because they can contain what was said |

The dashboard only offers the wake words whose models the image ships.

## Name the device

Home Assistant shows the device by its name. Until you choose one, the name is
the model and the last six characters of the MAC address, for example
`Echo Dot 2a914e`. To rename the device, open **System**, change **Name** and
select **Save**. The dashboard shows the new name at once. The **Node name**
under **System**, such as `pinna-2a914e`, stays: it is the device's ID for
Home Assistant and mDNS, not the name that people see. To change it too,
[change the hostname](#change-the-hostname).

Name the device before you add it to Home Assistant. Home Assistant takes a
new name the next time it connects, and shows it on the device page and in the
device list. The device asks it to reconnect as soon as no request, reply,
announcement, music or timer alarm is playing. The entity
IDs, such as `switch.echo_dot_2a914e_wake_sound`, and the entry under
**Settings > Devices & services > ESPHome** keep the name that the device had
when you added it. A name that you give the device in Home Assistant itself
wins over the device's.

## Set the time zone

The device keeps UTC until you choose a zone. Open **System**, then pick one
under **Time zone**. It is the zone of the device's own clock and logs, and
takes effect at once. If your browser is in another zone than the device, the page offers
**Use** and your browser's zone. Home Assistant keeps its own time zone.

## Change the hostname

The hostname is the device's name on your network, such as `pinna-2a914e`. It is
also its node name in ESPHome, which Home Assistant uses to find it. To change
it, open **Network**, enter a name under **Hostname** and select **Save**. A
hostname is one word of letters, digits and hyphens, for example `kitchen-dot`.
Give each device a different one.

The change takes effect at once, and the device's `.local` address changes with
it: `http://kitchen-dot.local/` replaces `http://pinna-2a914e.local/`, so
bookmarks to the old address stop working. If you opened the dashboard by the
old `.local` address, the browser moves to the new one by itself. Home
Assistant follows on its own, as soon as no request, reply, announcement, music
or timer alarm is playing. Your router's device list shows the new name after
the device next joins the Wi-Fi, for example after a restart. A reverse proxy
that reaches the device by its old `.local` name stops working: point it at the
new name, or better at the device's IP address.

After a name conflict on the network, the device can answer by another
`.local` name for a few minutes, such as `kitchen-dot3.local`. **Network** then
says which name it answers to. The device takes its own name back by itself,
at most once every ten minutes, so if the conflict comes back, it keeps the
other name for a while.

## Change the Wi-Fi network

Open **Network**. **Saved networks** lists the networks that the device
joins: the first one it finds, from the top. **Forget** removes one. Forgetting
the network in use disconnects the device, and if it was the only one, the
device is reachable over USB only.

To join another network, select **Scan** under **Join a network**, select the
network and enter its password. For a network that doesn't show its name,
select **Join a hidden network** and type the name exactly as the router has
it. The device joins open and WPA2 networks, WPA2/WPA3 networks included; its
Wi-Fi chip can't join networks that allow only WPA3, enterprise networks or WEP
networks.

Joining is a trial, so that a mistake can't strand the device on a network you
can't reach, such as a guest network that keeps its devices apart:

1. The device tries the network for up to a minute. If the password is wrong,
   or the network gives it no address, it goes back to the network it was on,
   and the page says why.
2. Once it is on the new network, it keeps the network as soon as someone
   opens the dashboard at its new address: connect a phone or computer to that
   network and open the device's `.local` address, which the page shows before
   you join, or the device's address from your
   router. You can also select **Keep it** on the page, for example over
   USB.
3. Until then, every page shows the network on trial and the time left. After
   5 minutes, on **Go back**, or when the device restarts, it goes back to its
   saved networks.

If you open the dashboard over Wi-Fi, the page loses the device when it
switches networks. Before you select **Join**, the page says where to find it,
including its MAC address to look for in your router.

## Reset the device

To start over, as if the device were just installed, open **System** and
select **Reset…** next to **Factory reset**. Type the device's name to confirm.
The device forgets:

- its name and every setting made on the dashboard or in Home Assistant
- its saved Wi-Fi networks
- the encryption key that Home Assistant set; a key that came with the image
  stays
- SSH keys added on the device, and its SSH host key.

It then restarts as it was installed. Its log stays, and so does the version
of Pinna it runs. If its image doesn't set up Wi-Fi, which a prebuilt release
doesn't, the device is reachable over USB only afterwards, at
`http://10.42.0.1/`: [connect it to Wi-Fi](/docs/flash/#connect-to-wi-fi)
again. If Home Assistant had set its key, Home Assistant shows it as
unavailable until you add it again.

## Tune the wake word

**Voice & sound > Sensitivity** opens the **Wake word** page. The slider sets
how easily the device wakes. Its hint shows the score that the setting needs.
The score is how sure the device is that it heard the wake word, from 0 to 1.
While you move the slider, the hint and the entries show what the new value
does.

**Recent wakes** lists the last 20 wakes and near misses. A near miss is a
sound that scored at least 0.20 but didn't wake the device; it may have
reached the setting and been rejected for another reason. Each entry shows
when it happened and its score; a wake also names the word that woke the
device. Each entry says what its score means at the current setting:

- **Woke**: the entry says which sensitivity ignores that score. If it wasn't
  you, select **Set to**.
- **Almost woke**: the entry says which sensitivity catches that score. If it
  was you, select **Set to**.

After the change, the same button says **Back to** and the old value. Select
it to undo the change.

The entry also says what else the new setting changes, for example "But it
also ignores 2 other wakes listed here". Check the times of those entries. If
ignoring a false wake also ignores your own wakes, or catching your near miss
also catches the TV, no setting fixes both. Move the device away from the TV or
the radio instead.

An entry without a button says why:

- **Ignored at this setting** or **Wakes at this setting**: the current
  setting already does what you want.
- **Too faint for any setting**: even 100 doesn't catch it. Speak closer, or
  with less noise around.
- **High enough at the time, but too quiet against the background or too short
  to count**: the device didn't accept it. Speak closer or louder, with less
  noise around.
- **Every setting wakes on this**: the score is 0.85 or more. If it wasn't
  you, move the device away from the TV or the radio, or turn off the wake word
  while it plays.

The list holds scores and wake word names, not what was said. It records only while the
device listens for the wake word. It starts again when `pinnad` or the device
restarts.

### Calibrate at the device

Select **Calibrate**, next to the slider, to tune the sensitivity from where
you usually stand, using the buttons on the device. The volume buttons become
feedback buttons while calibration runs: the ring shows the setting as a green
arc.

1. Say the wake word. When the device hears it, the ring flashes green and the
   wake sound plays, if you turned it on. Nothing else happens: no request
   starts, and a false wake costs nothing.
2. If it missed you, press **+** within 15 seconds. The sensitivity rises just
   enough to catch what it almost heard.
3. If it woke on something that wasn't you, such as the TV, press **−**, also
   within 15 seconds. The sensitivity drops just enough to ignore that.

The panel under the slider lists what each press did, for example
`+ raised the sensitivity 68 → 90, to catch a near miss at 0.48`. If a **+**
and a **−** would undo each other, because your wake and the false one score
about the same, the second press changes nothing: the ring flashes red at the
device, and the panel says why. No setting settles both, so move the device
away from the sound instead.

Select **Done**, press the Action button, or leave it 5 minutes without a
press to finish. Muting the microphones or turning the wake word off ends it
too. **Back to 68** returns the sensitivity to the value it had when you
started. Home Assistant has the same switch, **Wake word calibration**.
Calibration won't start while the microphones or the wake word are off, or
while a request or a timer alarm is active. It also works before Home
Assistant connects, because a calibration wake starts no request.

## Play music from a URL

1. Open **Voice & sound > Media sources**.
2. Under **Play a URL**, enter the address of an MP3, FLAC, Ogg Vorbis or WAV
   file or stream that starts with `http://` or `https://`, and select
   **Play**.
3. Open **Overview**. **Now playing** shows what plays, with **Pause**,
   **Resume** and **Stop**:
   - For a file, the title, artist and album from its tags, and the
     address's server and file name.
   - For internet radio, the title that the station sends with the music,
     and **From** with the station's name. Many stations send neither; then
     it shows the address.
   - Without a title, it shows **Playing** or **Paused** in its place.

The device fetches the address itself, from your network, and plays it as it
plays music from Home Assistant: it replaces what was playing once its audio
starts, gets quieter
while the device listens or speaks, and follows **Volume**. AAC, HLS and
playlist files such as `.m3u` and `.pls` don't play. For a radio station, use
its direct MP3 or Ogg stream address, not the playlist link.

Media that Home Assistant plays shows as **From Home Assistant**, without a
title or a station's name: Home Assistant converts the media for the device
first, and that removes them. To see a station's titles, play its address
from the dashboard instead.

For AirPlay and Bluetooth, **Now playing** shows the title, artist and album
that the phone or computer shares, and **From AirPlay** or **From Bluetooth**.
It has no **Pause**, because pausing ends the phone's session: use **Stop**,
or pause on the phone. **Music volume** sets how loud that music plays on the
device. The phone's own slider doesn't move.

If an address doesn't play, what was playing keeps playing. **Now playing**
shows **Couldn't play** under it, with the address and why, for example that
the server answered `HTTP 404 Not Found`. **Dismiss** removes the message:
with nothing playing, for everyone; while something plays, on this page
only, because clearing it there would stop the music.

**Mute music** silences the music only; replies, the wake sound and timer
alarms still sound. When the device isn't reachable, **Now playing** says
**Not reachable**, and what it shows may be out of date.

## Who can use the dashboard

The dashboard has no login. **Anyone on your network can use it.** That
includes renaming the device, setting its dashboard address, moving it to
another Wi-Fi network or forgetting the one it uses, making the device play an
address from your network, lighting the ring in any colour, the solid red of
the muted microphones included, restarting the device,
choosing the update channel, installing an update and switching to the version in the other slot. It also
includes seeing what plays: the title of the music a phone sends over AirPlay
or Bluetooth shows on **Now playing**. Only an update that
is signed with the device's release key installs, and the device goes back to
the previous version if the new one doesn't start. An install from a manifest
URL takes an older signed release too, so someone with a copy of one can
downgrade the device.

The dashboard itself has no switch to turn it off: on a page without a login,
anyone could turn it back on. Developers can turn it off with `listen = ""`
under `[web]` in `pinnad.toml`, as [Configure Pinna](/docs/configure/#web)
describes. `pinnactl` and the boot health check keep working then, because
they use the control API on `127.0.0.1:7470`.

## Set a dashboard address

The dashboard answers to IP addresses, to `localhost`, to names that end in
`.local` and to the device's hostname. That stops other web sites from
reaching it through your browser. If you open it by another name, for example
`https://dot.example.com/` that a reverse proxy serves, the browser shows a
short message from the device, such as
`This satellite does not answer to the name "dot.example.com".` To make the
device answer to that name:

1. Open the dashboard by the device's IP address or its `.local` name.
2. Open **Network**. Under **Dashboard address**, enter the full address, for
   example `https://dot.example.com/`, and select **Save**.

The address starts with `http://` or `https://`, has a host name or an IP
address and optionally a port, and has nothing after that: no user name,
password, path, query or fragment. The host must be one that other devices on
your network can reach, so `localhost`, loopback addresses, the USB link's
`10.42.0.x` addresses and link-local IPv6 addresses are refused. The device
stores it with a `/` at the end.
It works at once. **Remove** clears it. While the dashboard is open by the
address's own host name, it won't remove the address or change it to another
host, because the page would stop answering: open it by the IP address or the
`.local` name first.

Point the reverse proxy at the device's IP address, ideally one that your
router always gives it, rather than its `.local` name, which changes when you
change the hostname.

Other devices link to this address when you [switch between
devices](#switch-between-satellites). Without one, they use the device's
`.local` name or its IP address. Only one address fits: for a second name, such
as your router's name next to a reverse proxy's, developers can use
`allowed_hosts` under `[web]` in `pinnad.toml`. Anyone who can use the
dashboard can set the address, so check it now and then.

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

## What's next

- [Update Pinna](/docs/update/)
- [pinnactl and the control API](/docs/control-api/)
