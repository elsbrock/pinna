---
title: Play music with Sendspin
description: Turn on the Sendspin player, play from Music Assistant in step with the other speakers of a group, and find out why Music Assistant doesn't list the satellite.
---

Sendspin is the open protocol that Music Assistant uses for multi-room audio.
With Sendspin on, the satellite is a player in Music Assistant 2.10.7 or
later, and plays in step with the other Sendspin speakers of a group. The
music goes through the satellite's own speaker and audio processing, so the
wake word keeps working while it plays, and replies, announcements and timer
alarms play over it.

Sendspin is experimental, as the dashboard says: its specification is still a
draft, and a change to it can stop Music Assistant from playing to the
satellite until Pinna follows. It is tested against Music Assistant's
Sendspin server on a computer, not yet on an Echo, so how closely an Echo
keeps step is not measured yet. Sendspin is off until you turn it on.

## Before you begin

- You need Music Assistant 2.10.7 or later. Earlier releases speak an older
  form of Sendspin and can't connect.
- Music Assistant finds the satellite by mDNS, so it has to be on the same
  network as the satellite. On another network or VLAN, add the satellite by
  its address instead, as in
  [Add the satellite by its address](#add-the-satellite-by-its-address).
- Music Assistant connects to the satellite on TCP port 8928. A firewall
  between them has to allow that.

## Turn Sendspin on

1. Open the [dashboard](/docs/dashboard/), go to **Voice & sound** (**Voice**
   on a phone) and select **Media sources**, under **Sound**.

   If there is no **Sendspin** on it, the satellite runs a Pinna from before
   Sendspin: [update it](/docs/update/) first.
2. Turn on **Sendspin**. The page shows **Ready as** and the name that Music
   Assistant lists the satellite under.

If you use Home Assistant, it has the same switch: **Sendspin**, under
**Configuration** on the satellite's device page. A change in one shows in
the other.

The player uses the satellite's name, the one you set in
[Name the device](/docs/dashboard/#name-the-device). When you rename the
satellite, its connection to Music Assistant ends, which stops the music on
it. Music Assistant then connects again and reads the new name.

## Find the satellite in Music Assistant

Music Assistant lists the satellite as a player under its name, without
pairing or any other setup. Once Music Assistant connects, **Media sources**
shows **Connected to** and the server's name.

### Add the satellite by its address

Music Assistant on another VLAN or on a guest network doesn't see the
satellite, because mDNS doesn't cross between networks. Neither does Music
Assistant in a container without host networking. Give Music Assistant the
satellite's address instead:

1. On the dashboard, open **Network** and note the satellite's IP address
   under **Addresses**. In your router, reserve that address for the
   satellite, so that it doesn't change.
2. In Music Assistant, open the settings of the Sendspin provider, show its
   advanced settings, and add the address to **Manual IP addresses for
   discovery**. Music Assistant connects to TCP port 8928 at that address, so
   a firewall between the networks has to let it through.
3. On the dashboard, **Media sources** shows **Connected to** and the
   server's name.

### With AirPlay on as well

With [AirPlay](/docs/airplay/) on too, Music Assistant may list the satellite
more than once: as an AirPlay player, as an AirPlay player that Music
Assistant plays to through Sendspin, and as a Sendspin player. Use the
Sendspin player. Through AirPlay, the satellite keeps step only as well as
AirPlay's delay can be estimated, and a volume change on the satellite
doesn't reach Music Assistant. If you play to the satellite from Music
Assistant only, turn AirPlay off, and Music Assistant lists it once.

## Play music

In Music Assistant, play to the satellite, or add it to a group with other
Sendspin speakers and play to the group.

While Sendspin plays:

- The music replaces whatever the satellite played before: media from Home
  Assistant, AirPlay or Bluetooth.
- **Now playing** on the dashboard's **Overview** shows the title, artist and
  album that Music Assistant sends, and **From Music Assistant**. Anyone who
  can open the dashboard sees them.
- **Pause**, **Resume** and **Stop** on **Now playing**, Home Assistant's
  media player and a request by voice act on the whole group, as Music
  Assistant's own controls do. So does **Cancel request** in Home Assistant
  when no request runs and no alarm rings: it stops the group.
- After a pause from the satellite, **Now playing** shows the track as
  paused, and **Resume** starts the group's music again. After a stop, at the
  end of the queue, or after a pause in Music Assistant, it shows that
  nothing plays: resume the group in Music Assistant.
- **Music volume**, under **Now playing**, is the satellite's volume in Music
  Assistant, and so is the Home Assistant media player's volume. A change on
  either side shows on the other. **Mute music** mutes the satellite in Music
  Assistant too. The satellite keeps this volume when it restarts.
- **Volume**, on the dashboard, the satellite's buttons or the **Volume**
  entity in Home Assistant, sets how loud the satellite is overall. Music
  Assistant doesn't see it.

### Replies, announcements and timer alarms

While the satellite listens or works on a request, and while a reply, an
announcement or a timer alarm plays, the music gets quieter, as all music
does. The group plays on meanwhile: afterwards, the satellite plays from
where the group is, not from where it was.

### When another source plays

When something else plays on the satellite, such as a URL from Home
Assistant or the dashboard, AirPlay or Bluetooth, the satellite leaves its
group in Music Assistant. The rest of the group plays on. The same happens
when another source starts while the group waits paused from the satellite.
To play to the satellite again, add it back to the group in Music Assistant.

A group that stopped, ran out of music or was paused in Music Assistant
keeps the satellite. When that group plays again, it takes over from what
the satellite plays.

## Security

- **The satellite only plays.** Over Sendspin it takes audio, its volume and
  the track details from Music Assistant, and sends back its own volume and
  pause, play and stop. It sends no microphone audio: it asks only for
  Sendspin's player, controller and metadata roles.
- **There is no pairing yet.** While Sendspin is on, any device that can
  reach TCP port 8928 on the satellite can play to it, and replace what it
  plays, as with AirPlay: every device on the satellite's network, and on
  networks your router lets through. To narrow it, let only Music
  Assistant's address through to port 8928.
- **The music can be intercepted.** The connection is encrypted, but without
  pairing neither side can check who is at the other end. Someone who can sit
  between Music Assistant and the satellite, such as a router between your
  networks, could read or change the music.
- **The dashboard controls the group.** Anyone who can open the dashboard can
  pause and stop the whole group, not only the satellite. See
  [Who can use the dashboard](/docs/dashboard/#who-can-use-the-dashboard).
- Pairing through Home Assistant is planned: Home Assistant would hand the
  satellite's pairing key to Music Assistant, so that only your Music
  Assistant could play to it.

The satellite keeps its Sendspin identity and the volume across restarts. A
[factory reset](/docs/dashboard/#reset-the-device) gives it a new identity,
so Music Assistant may list it as a new player.

## Limits

- **In step on the Echo devices only, and not yet measured there.** On the
  Echo (2nd gen) and the Echo Dot (2nd gen), the satellite reads the
  speaker's hardware clock to know when each sample sounds. The delay from
  that clock to the sound counts as 0 until it is measured on each model, so
  the satellite can be off from the group by that delay. The virtual Echo Dot
  and other computers play, but not in step with the group: **Media
  sources** shows **Not in step**.
- **The wake word hears the rest of the group.** The echo canceller takes
  out only the satellite's own sound. The wake word and your requests hear
  other speakers of the group in the same room as they hear any other sound
  in the room.
- **One server at a time.** The satellite plays from one Music Assistant at a
  time. A second server that plays takes over from the first.
- **Music Assistant before 2.10.7 can't connect.** The dashboard says so.
- **Port 8928.** Music Assistant has to reach the satellite on TCP port 8928.
- **The buttons don't control the group.** The volume buttons set how loud
  the satellite is overall, as for every source.

## If Music Assistant doesn't list the satellite or doesn't connect

Look at **Voice & sound > Media sources** on the dashboard:

- **Ready as "…"**: the player runs, and no Music Assistant is connected.
  Check that Music Assistant is on the same network as the satellite, or
  [add the satellite by its address](#add-the-satellite-by-its-address), and
  that a firewall between them allows TCP port 8928.
- **Ready as "…"**, with "A server couldn't connect. Music Assistant must be
  2.10.7 or later.": a server closed the connection as Music Assistant before
  2.10.7 does. Update Music Assistant.
- **Connected to …**: Music Assistant is connected, and nothing plays. In
  Music Assistant, play to the satellite's name. If another source plays on
  the satellite, the page says so: play to the satellite's name again, or
  add it back to its group.
- **Paused**: you paused the group from the satellite. Resume it on **Now
  playing** or in Music Assistant.
- **Not running**, with "Port 8928 is in use.": another program on the
  satellite listens on port 8928. The satellite tries again every 5 seconds.
- **Not running**, with another reason: the satellite tries again every 5
  seconds, and **System > Logs** says more. The player's lines contain
  `sendspin:`.
- **The Sendspin switch is greyed out, and the page says the satellite has no
  working speaker**: **System > Logs** says why the speaker didn't start. The
  switch is also greyed out while the dashboard has lost the connection to
  the satellite.

## What's next

- [Audio](/docs/audio/): how the satellite plays and mixes music, replies and
  alarms.
- [Home Assistant entities](/docs/entities/): the **Sendspin** switch and the
  media player.
- [Prometheus metrics](/docs/metrics/#sendspin): how well the player keeps
  time.
