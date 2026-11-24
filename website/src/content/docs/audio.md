---
title: Audio
description: How Pinna processes the microphones and the speaker, so that the wake word works while the device plays audio.
---

The wake word has to work while the device speaks or plays music. For that,
Pinna removes the device's own sound from the microphone signal before the
wake word detection hears it. This page explains how the microphones and the
speaker are connected to make that work.

![The audio path: the microphones through PipeWire into pinnad's capture loop, and pinnad's two lanes through the mixer to the speaker.](/diagrams/audio.svg "The microphones run in PipeWire at 16 kHz, the speaker in pinnad at 48 kHz. Each echo canceller removes the speaker's sound from its own stream.")

## The hardware

The microphones and the speaker run on two separate clocks:

- **The speaker** is connected to an audio codec that only plays at 48 kHz.
- **The seven microphones** arrive through a separate interface at 16 kHz,
  as nine 24-bit channels: the seven microphones, then the left and the
  right channel of what the speaker plays.

Because the two clocks drift apart, Pinna doesn't route both through one
audio graph. The speaker stays with `pinnad`, and the microphones run in
PipeWire.

## Microphones

PipeWire runs on the satellite as a system service with a fixed graph at the
microphones' native 16 kHz. It turns the microphones into the one stream that
`pinnad` hears:

1. **The microphones, averaged.** It averages all seven microphones, after
   applying each microphone's factory calibration.
2. **Echo cancellation.** It removes the satellite's own sound with a linear
   echo canceller. As a reference, it uses the speaker's channel that the
   microphone interface delivers alongside the microphones, so the two can't
   drift apart. Many echo cancellers also mute the microphones while the
   satellite plays, which mutes you too. This one doesn't, so you can say the
   wake word over a reply or music: the reply stops, and music gets 20 dB
   quieter while you talk.
3. **`pinnad`** reads the stream in 20 ms frames and sets its gain
   automatically. The wake word detection, the requests to Home Assistant and
   the requests to your speech-to-text service all hear it.

PipeWire also works out which direction the person speaking is in, and shares
it with `pinnad`, which points the ring at them.

In tests on recordings, played through the same code on a computer, the echo
cancellation lowered the satellite's own music and speech by about 26 dB in
the speech range on an Echo Dot (2nd gen) at volume 55, and by about 13 dB on
an Echo (2nd gen) at volume 75. In a typical second it removed about 22 and
20 dB. The stream hasn't run on a satellite yet. In the first seconds of new
audio more echo remains: the canceller takes some seconds to adapt to the
room.

The automatic gain lifts quiet speech, such as someone talking across the
room, and lowers the gain for loud speech so that it doesn't clip. It aims for
speech at −24 dBFS, between 0 and +36 dB of gain, but lifts the room's
background noise no higher than −65 dBFS. That is below the level at which the
wake word detection starts to listen, so it doesn't take the lifted noise for
speech. In a quiet room the gain can rise further than in a noisy one. After
loud speech, music or a reply, the gain comes back up by 1 to 3 dB per second.

If the stream delivers nothing for a second, `pinnad` reads the raw
microphones and averages them itself, without echo cancellation, so the wake
word is less reliable while the satellite plays audio. **System > Logs** then
shows `microphone: the processed stream delivers nothing; recording the raw
lanes, without echo cancellation`. `pinnad` tries the stream again every
minute, and logs `microphone: the processed stream delivers again` once it
does.

If PipeWire isn't running, `pinnad` reads the microphones directly and
averages all seven, without echo cancellation. `pinnad`
makes this choice once, when it starts. If PipeWire fails later, `pinnad`
doesn't notice. The wake word is less reliable while the device plays audio.

## Speaker

`pinnad` plays everything itself, through ALSA at 48 kHz, so that no other
program can switch the amplifier on or off at the wrong moment. It mixes two
lanes:

- **Voice**: replies, announcements, the timer alarm and short sounds.
- **Media**: music and other audio from the media player, and live audio from
  [AirPlay](/docs/airplay/), [Bluetooth](/docs/bluetooth/) and
  [Sendspin](/docs/sendspin/).

While the voice lane plays, and while the device listens or works on a
request, the media lane gets 20 dB quieter, and returns afterwards.

The speaker of the Echo (2nd gen) and the Echo Dot (2nd gen) plays one
channel only, so `pinnad` mixes stereo audio to mono before it plays it.
Nothing panned to one side goes missing.

Live audio runs on the sender's clock, and the speaker on the device's own.
`pinnad` converts it to 48 kHz at a rate that follows the sender, so that it
always holds 100 ms of it: the delay stays the same however long the music
plays. Other programs on the device can play live audio the same way; see
[The stream socket](/docs/configure/#the-stream-socket).

Sendspin audio is timed instead: each piece carries the moment, on the
server's clock, at which its first sample has to sound. `pinnad` follows the
server's clock, and on the Echo (2nd gen) and the Echo Dot (2nd gen) reads
from the speaker's hardware clock when each sample it writes will sound. It
converts the audio at a rate that keeps the two in step. When they are too
far apart to correct gently, it skips audio or waits in silence, to start
again at the right sample. While the media lane is quieter for a reply, the
stream keeps its place, so the music continues where the rest of the group
is. With the `pipewire` backend, on the virtual Echo Dot and other
computers, there is no hardware clock to read: the audio plays, but not in
step with the group.

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

Sendspin has a volume of its own, under the volume too: the satellite's
volume in Music Assistant, on the curve that Sendspin defines. While
Sendspin plays, **Music volume** and the Home Assistant media player show
and set it, and Music Assistant's slider follows.

## What's next

- Set the volume and the reply volume on the [dashboard](/docs/dashboard/#what-the-dashboard-shows).
- For developers: the audio backends and the stream socket, in
  [Developer settings](/docs/configure/#audio).
