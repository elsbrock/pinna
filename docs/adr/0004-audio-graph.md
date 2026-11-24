# ADR 0004: The Echo Dot's microphone path runs in PipeWire; pinnad keeps the speaker

Status: accepted (2026-11-15). Supersedes the "audio goes straight to ALSA" part
of ADR 0002. Amended 2026-11-17: pinnad sets the microphone gain itself (AGC),
after echo cancellation; until then the gain was fixed. Amended 2026-11-21 by
ADR 0033: the wake word detector hears its own stream, `dot_wake`. Amended
2026-11-24: `dot_wake` is the only microphone stream. It averages all the
board's microphones and cancels the echo from the loopback lane the speaker
plays. WebRTC's echo canceller and pinnad's reference copy leave the graph,
the beamformer stays only to point the ring at the talker, and pinnad plays a
mono mix. Measured offline only; not run on
a device yet.

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
dot_mics (hw:0,24, graph clock, timer-driven, nine lanes)
  -> filter-chain: LADSPA pinna_wake (all nine lanes; the board says which are
     microphones and which loopback lane the speaker plays): the microphones
     averaged, a linear echo canceller from that loopback lane, the 80 Hz
     high-pass and +12.04 dB; the beamformer for the ring's direction only
  -> dot_wake (mono) -> pinnad (AGC, wake word, Home Assistant, standalone)
```

The speaker is not in PipeWire. pinnad drives it through ALSA at 48 kHz
(`EchoDotPipeWire` backend: aplay, its amp/codec sequences when the session
opens and closes,
and the speaker chain — a mono mix on boards whose speaker plays one channel,
a 150 Hz high-pass, the stock firmware's extra gain
above 70 % volume (up to +14.5 dB at 100 %) and a look-ahead peak limiter at
−0.1 dBFS). The echo canceller's reference comes from the microphones' own
loopback lanes, in their frames and on their clock, so pinnad no longer
writes a copy of what it plays. Until 2026-11-24 it wrote a 16 kHz copy into
WebRTC's reference sink, `dot_ref`; that copy came 2.05–2.16 s late through a
full pipe (Echo Dot (2nd gen), 2026-11-21) and its timing later jumped by 8 ms
twice within 12 s. The speaker's clock drifts against the CPU's (≈ +320 ppm),
so pinnad fits it from the PCM's hardware pointer
(`/proc/asound/card0/pcm*/sub0/status`) and paces the speaker at its measured
rate.
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

- **One stream: the average of all microphones, then one linear echo
  canceller** (amended 2026-11-24; until then: "beamform first, then mono echo
  cancellation", with LibreEcho's superdirective beamformer choosing its beam
  freely and WebRTC's canceller after it). `pinna_wake` (ADR 0033) makes the
  stream, and pinnad hears it for the wake word, Home Assistant and the
  standalone route. The board description (ADR 0032) says which lanes are
  microphones and which loopback lane carries what the speaker plays; the
  graph names neither. Measured offline with `micbench`
  (`crates/pinna-wakeword/examples/micbench`) on the nine-lane recordings of
  2026-11-21, and on copies with a talker added at known times and levels:
  - With a real talker in a quiet room (two takes, Echo Dot (2nd gen)), the
    mean of the live microphones stood 23.7–23.8 dB over the room in
    300–3400 Hz, the free beam 22.0–22.1 dB, the centre microphone
    17.2–17.3 dB. In 1.5–4.7 kHz: 19.2–20.6, 17.2–18.0 and 14.3–15.4 dB.
    TECHO5 found the same on a Dot: the average of seven beat the centre by
    0.9–2.5 dB, and steering added nothing.
  - The free beam steers at the loudest sound. With speech from the side as
    loud as the talker, the talker came out 6.3 dB under it, and 1 of 4 wake
    words was detected (the averages: 0 dB, 2 of 4).
  - A canceller after the free beam removes least, because each switch
    changes the echo path it learned: 6.2 dB of the Dot's music at volume 55,
    against 24.5 dB on the average.
- **The canceller's reference is the loopback lane the speaker plays**, which
  the board names: lane 8 on both MT8163 Echos, whose speaker reproduces the
  right channel only. On stereo music the best fixed linear filter (fitted
  offline per frequency over the music, 2048-point frames) removes 12.5 dB
  (Echo Dot (2nd gen), volume 55) and 13.3 dB (Echo (2nd gen), volume 75)
  from lane 8's reference, 0.8 and 0.9 dB from lane 7's, and 3.6 and 4.5 dB
  from their mean. On mono content every reference does as well (14–15 dB).
  The adaptive canceller follows the path as the music changes and removes
  more over a whole take (below).
- **pinnad plays a mono mix on boards whose speaker plays one channel**, so
  the left channel of stereo music is no longer lost, and both loopback lanes
  carry what the speaker plays.
- **No WebRTC canceller, no suppressor.** Its suppressor removes the talker
  with the echo during double talk (ADR 0033; TECHO5), and its linear filter
  removed less than Pinna's on the same input (16.6 against 24.5 dB, Dot's
  music, lane 8). During a request the satellite ducks music by 20 dB and
  stops its replies, so what the linear canceller leaves is quieter still.
- **The ring's direction** comes from the same beamformer as before, run
  inside `pinna_wake` for the direction only: it writes the snapshot, its
  audio goes nowhere.
- **If `dot_wake` ends without audio or delivers nothing for 1 s** (5 s
  before its first audio, while PipeWire links it at boot), pinnad records
  the raw lanes from `dot_mics` instead, averages the board's microphones
  itself with their calibration, the same high-pass and gain, and goes on
  without echo cancellation. After 60 s on the raw lanes, or when they end,
  it tries `dot_wake` again. Its `pw-record` asks for `node.dont-fallback`, so
  a missing `dot_wake` makes it exit instead of hearing another source. pinnad
  logs the switch once, and again when `dot_wake` delivers. A developer's
  `capture_target` turns the fallback off. Without PipeWire (the `EchoDot`
  backend), pinnad averages all of the board's microphones the same way
  (until 2026-11-24 four of them).
- Rejected:
  - **Keeping WebRTC for Home Assistant**, fed the average instead of the
    beam: two cancellers, the suppressor's double-talk loss, and pinnad's
    reference copy kept alive for it.
  - **The beam held on the talker for the turn** (TECHO5's design for
    requests): it gained 14–16 dB against speech from the side, but only on
    a synthetic talker without room reflections. It waits for counted takes
    with a real talker, and then belongs in `pinna_wake`.
  - **Leaving out lanes 3 and 4**, which hear 5–6 dB more of the Dot's speaker
    than the others: 1–6 dB better over the Dot's music on synthetic talkers,
    no difference on the Echo (2nd gen), and a quirk of one board.
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
  change over 115 s. A gain that moves before the echo canceller would change
  the echo path the canceller adapts to.
- **The speaker stays out of PipeWire**: full 48 kHz playback, and only
  pinnad's session starts and stops the PCM (and with it the amp) — no
  PipeWire graph restart can reach the speaker.

Measured on the device before 2026-11-24 (music through pinnad, speech band
300–3400 Hz, room noise present): after convergence WebRTC's canceller left
the echo 23–31 dB under the beamformer's output; PipeWire ~10% CPU. On
2026-11-21 the Echo Dot (2nd gen) measured 3.8 dB with pinnad's reference and
14.2 dB with the loopback lanes, both under the gate's 20 dB.
Offline, the one-stream graph (all microphones, lane 8, a step of 0.1) removes
25.8 dB of the Dot's music and speech at volume 55 and 12.7 dB of the Echo
(2nd gen)'s at volume 75 (the median second: 21.9 and 20.3 dB). The hardware gate (`just test-device`) requires
`dot_wake` to stay at least 10 dB under the raw microphones' mean while music
plays. The canceller costs about 19 % of a core on an Echo Dot (2nd gen)
(1.5 ms per 8 ms of audio, measured with a test build on 2026-11-21); the
beamformer about 5 %, estimated from host timings against that.

## Consequences

- pinnad drives ALSA directly for both directions (`EchoDot` backend) when the
  PipeWire socket is absent at startup. The choice is made once. A graph that
  runs without `dot_wake` is caught by the raw-lane fallback; one that fails
  altogether is not.
- The first seconds of new audio are not fully cancelled while the canceller
  adapts: at its step of 0.1 it gains about 3 dB a second on noise (ADR 0033).
- Leftover echo during replies and music counts as noise or speech for the
  AGC and lowers the gain; it comes back at 1 dB/s afterwards if it counted
  as speech, 3 dB/s if only as noise. The
  hardware gate's echo measurement (`dot_mics` against `dot_wake`) is
  inside PipeWire and doesn't see the AGC.
- Laptops keep using the plain `PipeWire` backend. The VM images ran without
  an audio backend at first; since ADR 0014 the virtual Echo Dot uses the
  plain `PipeWire` backend too, set in its `vm.toml`.
- The beamformer runs in PipeWire's process, inside `pinna_wake`, so pinnad
  cannot ask it which beam it follows. After every cycle the plugin writes a
  seqlock snapshot
  (selected beam, per-beam power) to `/run/pipewire/pinna-beam`
  (`pinna_audio::beamview`), and pinnad maps it read-only to point the
  listening ring at the talker. The file only ever grows, so a reader's mapping
  stays valid when PipeWire restarts. A snapshot older than 500 ms means the
  graph has stopped. The ring shows the plain listening pulse until a
  confident snapshot arrives, and again after a second without one.
