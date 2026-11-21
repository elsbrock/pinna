# ADR 0004: The Echo Dot's microphone path runs in PipeWire; pinnad keeps the speaker

Status: accepted (2026-11-15). Supersedes the "audio goes straight to ALSA" part
of ADR 0002. Amended 2026-11-17: pinnad sets the microphone gain itself (AGC),
after echo cancellation; until then the gain was fixed.

## Context

The wake word has to keep working while the Dot talks or plays music, which
needs echo cancellation with the speaker signal as reference. The Dot's audio
hardware is two unrelated clock domains: the speaker is an I2S codec (AFE DL1,
48 kHz only, read/write access only, 128 ms buffer) and the seven microphones
come in over SPI from an FPGA (9 lanes, 16 kHz only, packed 24-bit, bursty
pointer, period 640, buffer ≤ 2560 frames).

What was tried on the device and rejected:

- **Speaker clocks the graph, microphone follows** (rate-matched): the SPI
  capture driver's bursty pointer sends the follower into a re-prepare loop that
  restarts the speaker over and over (audible clicks) and stalls capture.
- **Speaker and microphone in one graph, microphone clocks it**: stable, but a
  PipeWire graph runs at its driver's rate, so everything — music included —
  went through 16 kHz (8 kHz bandwidth).
- **Presenting the microphone as 48 kHz** (`plughw`, explicit ALSA `rate`
  device): PipeWire disables plug's automatic conversions, and the `rate`
  plugin finds no usable configuration for this capture device.
- **Per-microphone echo cancellation**: PipeWire's WebRTC plugin (Debian's
  webrtc-audio-processing 1.x) processes a single capture channel.

## Decision

PipeWire and WirePlumber run as system services on the device
(`pinna-pipewire.service`, `pinna-wireplumber.service`, only on MT8163; the
x86_64 image runs them with a virtual graph instead, ADR 0014), with a
static microphone graph (`/etc/pipewire/pipewire.conf.d/10-echo-dot.conf`, no
device monitors) at the microphone's native 16 kHz:

```
dot_mics (hw:0,24, graph clock, IRQ mode)
  -> filter-chain: LADSPA pinna_beam (calibration, gain, beamformer) -> dot_beam (mono)
  -> module-echo-cancel (WebRTC, mono) -> dot_mic_aec -> pinnad (AGC, wake word, Home Assistant)
pinnad -> dot_ref (16 kHz mono copy of everything it plays) = echo reference
```

The speaker is not in PipeWire. pinnad drives it through ALSA at 48 kHz
(`EchoDotPipeWire` backend: aplay, its amp/codec sequences when the session
opens and closes,
and the speaker chain — a 150 Hz high-pass, the stock firmware's extra gain
above 70 % volume (up to +14.5 dB at 100 %) and a look-ahead peak limiter at
−0.1 dBFS), and writes a 16 kHz mono copy of every chunk it plays into
`dot_ref` through a bounded queue (dropped, never blocking, if PipeWire lags).
A chunk is also dropped while more than 40 ms of reference still waits in
`pw-play`'s input pipe: the speaker writes 100 ms ahead, so the reference
reaches the canceller before its echo. Without that bound the pipe (64 KiB,
2 s) filled while `pw-play` started and never drained. Measured on an Echo
Dot (2nd gen), 2026-11-21: the reference came 2.05–2.16 s after its echo, and
`dot_mic_aec` was as loud as `dot_beam` at every volume.
The two clocks drift apart (speaker ≈ +320 ppm, microphones ≈ +5 ppm against
the CPU), so pinnad fits both from the PCMs' hardware pointers
(`/proc/asound/card0/pcm*/sub0/status`), paces the speaker at its measured rate
and resamples the reference from the speaker clock to the microphone clock.
WebRTC's delay estimator only absorbs the fixed offset between the two paths.
The amp cracks every time it powers up: about −34 dBFS at the microphones,
~30 dB above a quiet room, whatever the codec, MFP2 and headphone-driver state
(measured; −20 dBFS if the codec is still off). Switching it off is silent.
The kernel powers it up itself when the PCM starts (the AFE's trigger raises
the amp GPIO, and the codec releases MFP2 at the end of the ~1.2 s prepare),
so pinnad's amp and MFP2 controls after the PCM reads `RUNNING` change nothing
physically; at close they do (mute, then amp off). pinnad therefore opens the
speaker session when it starts and keeps it open on silence: the amp powers up
once, at start-up, and replies don't wait for the prepare. A failed open is
retried every 5 s. An underrun restarts the PCM and so cycles the amp;
aplay's underrun messages are logged as warnings.

- **Beamform first, then mono echo cancellation**: the usual design for small
  arrays and far cheaper than per-microphone AEC. The beamformer is LibreEcho's
  superdirective fixed-beam design, ported to Rust (within 1 LSB of the C on
  ≥ 99.9 % of samples) and loaded into
  PipeWire as a LADSPA plugin (`crates/pinna-ladspa`).
- **Automatic gain after echo cancellation, in pinnad** (amended
  2026-11-17): `pinna_audio::agc` sets the gain on pinnad's 20 ms frames
  before the pre-roll, the wake word and the stream to Home Assistant, on
  every backend. It tracks the noise floor (falls at once, rises 3 dB/s) and
  the level of speech (frames 9 dB or more above the floor; without speech it
  decays at 1 dB/s), and aims for the
  lowest of: speech at −24 dBFS RMS, room noise at −65 dBFS, and +36 dB;
  never below 0 dB. The gain rises at 3 dB/s and falls at 30 dB/s, ramped
  across each frame, and drops within a frame so that peaks stay at or below
  −1 dBFS (input already louder than that passes at 0 dB). It starts at the
  old fixed gain (+12 dB on this graph, 0 dB elsewhere). `capture_gain_db` in
  `pinnad.toml` replaces it with a fixed gain.
  The noise ceiling sits below the wake word detector's energy VAD, which
  needs more than about −58.5 dBFS whatever the noise (an absolute floor from
  LibreEcho's VAD, which Pinna ports): lifted noise never opens it, so its
  speech-over-noise rule decides. Measured (Echo (2nd gen), living room,
  2026-11-17): the detector's scores don't depend on level (the
  `alexa-twice` fixture scores 0.997–1.000 from 0 to −42 dB, nothing at
  −45 dB, where it falls under that floor). With the old fixed gain, speech
  from about 4 m reached the detector at speech-band peaks of −35 to −53 dB,
  close to the floor, and about 30 dB below speech from 0.5 m.
  Rejected: WebRTC's own AGC in the echo canceller (`webrtc.gain_control`).
  PipeWire 1.4.2 doesn't expose its settings, and on the same device it added
  a constant +7 dB to near speech, far speech and room noise alike, with no
  change over 115 s. A gain before the echo canceller (in `pinna_beam`) would
  change the echo path the canceller adapts to.
- **The speaker stays out of PipeWire**: full 48 kHz playback, and only
  pinnad's session starts and stops the PCM (and with it the amp) — no
  PipeWire graph restart can reach the speaker.

Measured on the device (music through pinnad, speech band 300–3400 Hz, room
noise present): after convergence the echo is 23–31 dB lower after cancellation
than at the beamformer output (~6 dB before the clock tracking); the reference
stream needs 100 ms of buffering to ride out feeder jitter; PipeWire ~10% CPU.
The hardware gate (`just test-device`) requires at least 20 dB.

## Consequences

- pinnad drives ALSA directly for both directions (`EchoDot` backend) when the
  PipeWire socket is absent at startup. The choice is made once: a graph that
  fails later, or runs without `dot_mic_aec`, is not detected.
- The first seconds of new audio are not fully cancelled while WebRTC adapts.
- Leftover echo during replies and music counts as noise or speech for the
  AGC and lowers the gain; it comes back at 1 dB/s afterwards if it counted
  as speech, 3 dB/s if only as noise. The
  hardware gate's echo measurement (`dot_beam` against `dot_mic_aec`) is
  inside PipeWire and doesn't see the AGC.
- Laptops keep using the plain `PipeWire` backend. The VM images ran without
  an audio backend at first; since ADR 0014 the virtual Echo Dot uses the
  plain `PipeWire` backend too, set in its `vm.toml`.
- The beamformer runs in PipeWire's process, so pinnad cannot ask it which
  beam it follows. After every cycle the plugin writes a seqlock snapshot
  (selected beam, per-beam power) to `/run/pipewire/pinna-beam`
  (`pinna_audio::beamview`), and pinnad maps it read-only to point the
  listening ring at the talker. The file only ever grows, so a reader's mapping
  stays valid when PipeWire restarts. A snapshot older than 500 ms means the
  graph has stopped. The ring shows the plain listening pulse until a
  confident snapshot arrives, and again after a second without one.
