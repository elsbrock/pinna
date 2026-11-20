---
title: Play music
description: Play a file or an internet radio stream from its address, see what plays, and find the other ways to play music on the satellite.
---

The satellite plays music from these sources:

- An address that you enter on the dashboard: a file or an internet radio
  stream. See [Play music from a URL](#play-music-from-a-url).
- Home Assistant, on the satellite's
  [media player](/docs/entities/#media-player).
- A phone or a computer, over [AirPlay](/docs/airplay/) or
  [Bluetooth](/docs/bluetooth/).
- Music Assistant, over [Sendspin](/docs/sendspin/), in step with the other
  speakers of a group.
- In standalone mode, a radio station that you ask for by name. See
  [Choose what the brain may do](/docs/standalone/#choose-what-the-brain-may-do).

**Now playing**, on the dashboard's **Overview**, shows what plays.

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

For [Sendspin](/docs/sendspin/), **Now playing** shows the title, artist and
album that Music Assistant sends, and **From Music Assistant**. **Pause**,
**Resume** and **Stop** act on the whole group in Music Assistant. After
**Pause**, **Now playing** keeps the track, as paused, until the group plays
again. While Sendspin plays, **Music volume** and **Mute music** are the
device's volume and mute in Music Assistant, and Music Assistant's slider
follows them.

If an address doesn't play, what was playing keeps playing. **Now playing**
shows **Couldn't play** under it, with the address and why, for example that
the server answered `HTTP 404 Not Found`. **Dismiss** removes the message:
with nothing playing, for everyone; while something plays, on this page
only, because clearing it there would stop the music.

**Mute music** silences the music only; replies, the wake sound and timer
alarms still sound. When the device isn't reachable, **Now playing** says
**Not reachable**, and what it shows may be out of date.

## What's next

- [Play music with AirPlay](/docs/airplay/).
- [Play music over Bluetooth](/docs/bluetooth/).
- [Play music with Sendspin](/docs/sendspin/).
