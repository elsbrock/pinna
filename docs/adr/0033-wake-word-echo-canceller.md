# ADR 0033: The wake word gets its own echo canceller

Status: proposed (2026-11-21), accepted (2026-11-21). Amends ADR 0004 (the
Echo Dot's microphone graph). Not built. The measurements below come from one
talker at one spot.

## Context

Saying the wake word while the satellite talks should interrupt it. On an
Echo Dot (2nd gen) it doesn't. Measured on 2026-11-21 on one Dot running
0.5.0-beta.2 plus commit 04cbc41, which fixed the echo reference that arrived
2 s late (ADR 0004). One talker stood 1 m away and said "Alexa" five times per
take over a spoken reply, at sensitivity 68:

| Volume | Nothing playing | 50 | 60 | 70 |
|---|---|---|---|---|
| Detected on `dot_mic_aec` | 3 of 5 | 0 of 5 | 0 of 5 | 1 of 5 |

What the recordings show:

- **WebRTC's suppressor mutes the talker.** PipeWire's `module-echo-cancel`
  runs WebRTC's AEC3, a linear filter followed by a suppressor. At volume 50
  the remaining echo was −81 dBFS. Even so, the talker came out 10–15 dB weaker
  than with nothing playing, and no wake word scored above the threshold.
  PipeWire 1.4.2 passes only on/off settings to WebRTC. Neither the
  suppressor's tuning nor AEC3's linear output, taken before the suppressor,
  can be reached.
- **AEC3's linear filter alone removes little.** Replayed offline with the same
  library and PipeWire's settings (`tools/aec-replay`), it removed about 3 dB
  at volume 70 with pinnad's reference.
- **The microphones carry their own echo reference.** Lanes 7 and 8 of
  `dot_mics` carry what the speaker plays, in the same stream as the
  microphones: −33 dBFS at volume 70, 3 ms ahead of its echo. Nothing uses them
  today.
- **Microphone lane 1 was off after boot**, on this Dot and on an Echo (2nd
  gen). One ADC input switch didn't hold the value pinnad wrote, so the lane
  heard nothing. Writing it again brought the lane back on both; pinnad now
  checks its microphone settings once capture runs (commit e6c9fda). The
  takes at 50, 60 and 70 below were recorded with lane 1 working.
- **Holding the beam during playback changes little.** A separate study on
  2026-11-21 found the cancelled echo within about 2 dB of a free-running beam.

A prototype was run offline on the same recordings: a linear echo canceller
with no suppressor, on the mean of lanes 0, 3 and 4, with lane 7 as the
reference. On the beam's four microphones (lanes 0, 1, 3 and 4) it detected
the same at 50 and 60, and 3 of 5 at 70 (4 of 5 with the playback rule). It is a partitioned-block frequency-domain NLMS filter with 2048
taps, and it runs two filters so that double talk can't make it diverge.

| Volume | Nothing playing | 50 | 60 | 70 |
|---|---|---|---|---|
| Detected | 3 of 5 | 3 of 5 | 3 of 5 | 4 of 5 |

With the detector's playback rule (`Detector::set_playback`, one supporting
score instead of two), 70 detects 5 of 5. On the spoken reply alone, at volumes
50 to 80, it removed 14–20 dB of echo, leaving −54 to −66 dBFS. Nothing
scored above 0.011, so there were no false detections. TECHO5's notes on the
same hardware came to the same conclusion: its NLMS canceller suited the wake
word better than WebRTC, whose suppressor removed the talker with the echo.

## Decision

On the Echo Dot (2nd gen), the wake word detector gets its own microphone
stream with its own echo canceller. Home Assistant keeps the WebRTC output.

- **A new LADSPA plugin, `pinna_wake`**, in `pinna-ladspa` beside
  `pinna_beam`. Its inputs are microphone lanes 0–6 and loopback lane 7, and it
  has one mono output. It averages the calibrated microphones in its mask,
  then subtracts a linear estimate of the echo, computed from lane 7:
  - frequency-domain NLMS: 128-sample blocks and 16 partitions, so 2048 taps
    or 128 ms;
  - two filters: one adapts all the time, and the other produces the output and
    takes the first one's coefficients only when they cancel more;
  - no suppressor and no noise suppression;
  - then the beam's 80 Hz high-pass and +12.04 dB.
- **The mask** is the beam's four microphones (lanes 0, 1, 3 and 4), set in the
  graph like the beam's.
- **The graph** gets a second filter-chain, `dot_wake`, a mono Audio/Source.
  Like `dot_beam`, it captures `dot_mics` passively, lanes 0–7.
- **pinnad**, on the EchoDotPipeWire backend, feeds the wake word detector from
  `dot_wake`, with an automatic gain of its own. Home Assistant, the standalone
  route and the pre-roll keep `dot_mic_aec`. Once the wake word interrupts a
  reply the satellite stops playing, so WebRTC's suppressor no longer has
  anything to suppress while the request is heard.
- **Unchanged:** `module-echo-cancel`, pinnad's reference to `dot_ref`, the
  beam and the volume. Other models keep a single stream.

Rejected:

- **A volume ceiling while the satellite can be interrupted.** Through WebRTC,
  the wake word failed at 50 and 60 as well.
- **A lower threshold during playback.** Under the suppressor most peak scores
  were 0.00, below any threshold.
- **Ducking after a near miss.** The score rises only in the last 100–150 ms
  before a detection (TECHO5), too late to help the word that was heard.
- **AEC3's linear output, or a tuned suppressor.** PipeWire 1.4.2 exposes
  neither, so Pinna would have to run WebRTC itself.
- **Holding the beam.** About 2 dB.
- **The canceller on `dot_beam`.** In the prototype at volume 60 it detected 1
  of 5 there, against 3 of 5 on the microphone mean. The beam switches during
  playback, and each switch changes the echo path the filter has learned.
- **The canceller in pinnad, with pinnad's reference.** pinnad receives no
  raw microphones, and its reference would have to be timed against them. In
  the plugin, the loopback lane comes in the same frame as the microphones.

## Consequences

- PipeWire runs a second adaptive filter. Its cost on the MT8163 is measured
  before the change is merged; the graph runs at about 10 % CPU today.
- On the Dot, pinnad reads two microphone streams. The level that each wake
  word logs for ADR 0026 then comes from `dot_wake`, with that stream's own gain.
- The wake word's stream keeps whatever echo the linear filter leaves. The
  detector's VAD and threshold handle it: no false detections in the
  measurements above.
- Tests:
  - unit tests of the canceller: it converges on a known echo path, and it
    doesn't diverge under double talk;
  - a check in `just test-device` that `dot_wake` removes playback echo;
  - after the build, the same five-cue takes on the Dot at volumes 50, 60, 70
    and 80, to confirm the offline numbers.
- ADR 0004 and the website's audio pages describe the second stream once it is
  built.
