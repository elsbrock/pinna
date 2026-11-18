---
title: Audio
description: How Pinna processes the microphones and the speaker, so that the wake word works while the device plays audio.
---

The wake word has to work while the device speaks or plays music. For that,
Pinna removes the device's own sound from the microphone signal before the
wake word detection hears it. This page explains how the microphones and the
speaker are connected to make that work.

## The hardware

The microphones and the speaker run on two separate clocks:

- **The speaker** is connected to an audio codec that only plays at 48 kHz.
- **The seven microphones** arrive through a separate interface at 16 kHz,
  as nine 24-bit channels.

Because the two clocks drift apart, Pinna doesn't route both through one
audio graph. The speaker stays with `pinnad`, and the microphones run in
PipeWire.

## Microphones

PipeWire runs on the device as a system service with a fixed graph at the
microphones' native 16 kHz:

1. **Beamformer.** A LADSPA plugin, `pinna_beam`, combines the microphones
   into one channel with a fixed beamforming design, which reduces room noise.
   It also applies the microphones' calibration and gain. It shares which
   direction it is listening in with `pinnad`, which points the light ring at
   the person speaking.
2. **Echo cancellation.** PipeWire's WebRTC echo canceller removes the
   device's own playback from that channel. As a reference, it uses a copy of
   everything `pinnad` plays.
3. **`pinnad`** reads the result in 20 ms frames and sets its gain
   automatically, for the wake word detection and for the requests it sends,
   to Home Assistant or to your speech-to-text service.

On the device, the echo cancellation lowers the device's own music by 23 to
31 dB in the speech range, after it has adapted to the room. In the first
seconds of new audio, some echo remains.

The automatic gain lifts quiet speech, such as someone talking across the
room, and lowers the gain for loud speech so that it doesn't clip. It aims for
speech at −24 dBFS, between 0 and +36 dB of gain, but lifts the room's
background noise no higher than −65 dBFS. That is below the level at which the
wake word detection starts to listen, so it doesn't take the lifted noise for
speech. In a quiet room the gain can rise further than in a noisy one. After
loud speech, music or a reply, the gain comes back up by 1 to 3 dB per second.

If PipeWire isn't running, `pinnad` reads the microphones directly and
averages four of them, without beamforming or echo cancellation. `pinnad`
makes this choice once, when it starts. If PipeWire fails later, `pinnad`
doesn't notice. The wake word is less reliable while the device plays audio.

## Speaker

`pinnad` plays everything itself, through ALSA at 48 kHz, so that no other
program can switch the amplifier on or off at the wrong moment. It mixes two
lanes:

- **Voice**: replies, announcements, the timer alarm and short sounds.
- **Media**: music and other audio from the media player, and live audio from
  [AirPlay](/docs/airplay/) and [Bluetooth](/docs/bluetooth/).

While the voice lane plays, and while the device listens or works on a
request, the media lane gets 20 dB quieter, and returns afterwards.

Live audio runs on the sender's clock, and the speaker on the device's own.
`pinnad` converts it to 48 kHz at a rate that follows the sender, so that it
always holds 100 ms of it: the delay stays the same however long the music
plays. Other programs on the device can play live audio the same way; see
[The stream socket](/docs/configure/#the-stream-socket).

Replies and announcements play while they download. If the audio arrives
slower than it plays, the reply pauses in silence until enough audio is
there, and then continues. Nothing is skipped. After each pause it waits for
more audio before it continues, so a slow reply pauses less often.

## Volume

The volume runs from 0 to 100 and is applied digitally. 0 is silent, 1 is about
−60 dB and 100 is about −12 dB. The Echo Dot's speaker gets 6 dB more, and
above 70 also the extra gain of its stock firmware, up to +14.5 dB at 100. A
limiter holds the peaks just under full scale, so loud replies and music
don't clip.

**Reply volume** sets how loud the voice lane is: replies and announcements,
and also the timer alarm and short sounds. It uses the same scale as the
volume, and the voice lane is never louder than the volume: with the volume
at 30, a reply volume of 50 plays at 30. Until someone sets it, replies
follow the volume. On the [dashboard](/docs/dashboard/), turn
**Replies follow Volume** off under **Voice & sound**: **Reply volume**
appears, set to the current volume. Turning the switch back on makes replies
follow the volume again, which Home Assistant's **Reply volume** number can't
do.

**Stream volume** lowers live audio from AirPlay and Bluetooth the same way.
It follows the sending phone's or computer's volume. While a stream plays,
**Music volume**, under **Now playing** on the dashboard's **Overview**, and
the Home Assistant media player show and set the stream volume; the volume
itself stays where it is.

## What's next

- [Use the dashboard](/docs/dashboard/) to set the volume and the reply volume.
- For developers: the audio backends and the stream socket, in
  [Developer settings](/docs/configure/#audio).
- The full reasoning, with the alternatives that were tried, is in
  [ADR 0004](https://github.com/elsbrock/pinna/blob/main/docs/adr/0004-audio-graph.md).
