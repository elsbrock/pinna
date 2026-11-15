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
3. **`pinnad`** reads the result in 20 ms frames, for the wake word detection
   and for the requests to Home Assistant.

On the device, the echo cancellation lowers the device's own music by 23 to
31 dB in the speech range, after it has adapted to the room. In the first
seconds of new audio, some echo remains.

If PipeWire isn't running, `pinnad` reads the microphones directly and
averages four of them, without beamforming or echo cancellation. `pinnad`
makes this choice once, when it starts. If PipeWire fails later, `pinnad`
doesn't notice. The wake word is less reliable while the device plays audio.

## Speaker

`pinnad` plays everything itself, through ALSA at 48 kHz, so that no other
program can switch the amplifier on or off at the wrong moment. It mixes two
lanes:

- **Voice**: replies, announcements, the timer alarm and short sounds.
- **Media**: music and other audio from the media player. This lane also takes
  live audio on a Unix socket, the `stream_socket` setting. Nothing on the
  device feeds it yet.

While the voice lane plays, and while the device listens or works on a
request, the media lane gets 20 dB quieter, and returns afterwards.

## Volume

The volume runs from 0 to 100 and is applied digitally. 0 is silent, 1 is about
−60 dB and 100 is about −12 dB. The Echo Dot's speaker gets 6 dB more, and
above 70 also the extra gain of its stock firmware, up to +14.5 dB at 100. A
limiter holds the peaks just under full scale, so loud replies and music
don't clip.
**Reply volume** lowers the voice lane relative to the volume, and is never
louder than the volume.

## Audio backends

The `[audio] backend` setting in the [configuration](/docs/configure/)
chooses how `pinnad` reaches the audio hardware. Keep it at `"auto"`:

| Backend | Used when |
| --- | --- |
| `echodot-pipewire` | On the Echo (2nd gen) and Echo Dot (2nd gen), when PipeWire runs. The normal case |
| `echodot` | The same, without PipeWire. Microphones and speaker through ALSA |
| `pipewire` | On other computers, through PipeWire, when `$XDG_RUNTIME_DIR/pipewire-0` exists. Virtual machine images run without an audio backend |
| `wav:<path>` | Reads the microphone signal from a WAV file. No speaker. For tests |
| `none` | No audio. The wake word and voice requests are off |

## What's next

- [Configure Pinna](/docs/configure/)
- The full reasoning, with the alternatives that were tried, is in
  [ADR 0004](https://github.com/elsbrock/pinna/blob/main/docs/adr/0004-audio-graph.md).
