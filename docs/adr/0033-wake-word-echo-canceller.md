# ADR 0033: The wake word gets its own echo canceller

Status: proposed (2026-11-21), accepted (2026-11-21), built (2026-11-21), with
the tests that run without hardware; not run on a device yet. Amends ADR 0004
(the Echo Dot's microphone graph). The measurements below come from one talker
at one spot. Amended 2026-11-24 (ADR 0004): `dot_wake` is the satellite's only
microphone stream; it averages all the board's microphones, takes its
reference from the loopback lane the speaker plays (lane 8, not 7), adapts
with a step of 0.1 instead of 0.5, and carries the ring's direction. Measured
offline only.

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

A linear echo canceller with no suppressor was then run offline on the same
recordings, with lane 7 as the reference: a partitioned-block frequency-domain
NLMS filter with 2048 taps, running two filters so that double talk can't make
it diverge. First as a prototype on the mean of lanes 0, 3 and 4, then on the
beam's four microphones (lanes 0, 1, 3 and 4), and finally as built
(`pinna_audio::echocancel`, starting cold) on those four:

| Detected | Nothing playing | 50 | 60 | 70 |
|---|---|---|---|---|
| Prototype, lanes 0, 3, 4 | 3 of 5 | 3 of 5 | 3 of 5 | 4 of 5 |
| Prototype, lanes 0, 1, 3, 4 | | 3 of 5 | 3 of 5 | 3 of 5 |
| As built, lanes 0, 1, 3, 4 | | 5 of 5 | 2 of 5 | 4 of 5 |

With the detector's playback rule (`Detector::set_playback`, one supporting
score instead of two), the prototype on lanes 0, 3 and 4 detects 5 of 5 at 70,
and on lanes 0, 1, 3 and 4 4 of 5. On the spoken reply alone, at volumes 50 to
80, it removed 14–20 dB of echo, leaving −54 to −66 dBFS. Nothing scored above
0.011, so there were no false detections. TECHO5's notes on the same hardware
came to the same conclusion: its NLMS canceller suited the wake word better
than WebRTC, whose suppressor removed the talker with the echo.

## Decision

On the MT8163 models, the wake word detector gets its own microphone stream
with its own echo canceller. Until 2026-11-24 Home Assistant kept the WebRTC
output; since then this stream is the only one (ADR 0004). The Echo Dot (2nd
gen) and the Echo (2nd gen) share one graph, so both get it. Both deliver the
same nine lanes; recordings with playback on both (2026-11-21) show their
speakers play lane 8's channel.

- **A new LADSPA plugin, `pinna_wake`**, in `pinna-ladspa`. Its inputs are
  all nine lanes of `dot_mics`, and it has one mono output. It reads the board
  description (ADR 0032) when PipeWire loads it: which lanes are microphones,
  and which loopback lane carries what the speaker plays (lane 8 on both
  MT8163 Echos; amended 2026-11-24, until then lane 7). It averages the
  calibrated microphones in its mask, then subtracts a linear estimate of the
  echo, computed from that lane:
  - frequency-domain NLMS: 128-sample blocks and 16 partitions, so 2048 taps
    or 128 ms;
  - a step of 0.1 (amended 2026-11-24, until then 0.5; see below);
  - two filters: one adapts all the time, and the other produces the output and
    takes the first one's coefficients only when they cancel more;
  - no suppressor and no noise suppression;
  - then the 80 Hz high-pass and +12.04 dB.
- **The mask** is all of the board's microphones (amended 2026-11-24, until
  then the beam's four, lanes 0, 1, 3 and 4). The graph can narrow it with the
  `Mic mask` control.
- **The ring's direction**: the plugin also runs the beamformer on the
  board's speech microphones, only to write the snapshot that points the ring
  at the talker (ADR 0004). Its audio goes nowhere.
- **The graph** has one filter-chain, `dot_wake`, a mono Audio/Source that
  captures all nine lanes of `dot_mics`.
- **pinnad**, on the EchoDotPipeWire backend, hears `dot_wake` for the wake
  word, Home Assistant, the standalone route and the pre-roll (amended
  2026-11-24; until then they kept WebRTC's `dot_mic_aec`).
- **The fallback**: when `dot_wake` ends without audio (`pw-record` exits
  at once when the node is missing, because it asks for
  `node.dont-fallback`), or delivers nothing for 1 s (5 s before its first
  audio, while PipeWire links it at boot), pinnad records the raw lanes from
  `dot_mics` and averages the board's microphones itself, without echo
  cancellation. After 60 s on the raw lanes, or when they end, it tries
  `dot_wake` again. pinnad logs the switch once, and again when `dot_wake`
  delivers. A developer who points pinnad's capture elsewhere
  (`capture_target`) turns the fallback off. The filter-chain is marked
  `nofail`, so a missing plugin leaves `dot_mics` running.
- **Unchanged:** the volume. The other backends (the virtual Echo Dot, a PC)
  keep a single stream of their own.

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

### Amended 2026-11-24: lane 8 and a step of 0.1

Measured offline on the nine-lane recordings of 2026-11-21, some with a
talker added at known times and levels, with `micbench`
(`crates/pinna-wakeword/examples/micbench`) and, for the best fixed filter,
a least-squares fit per frequency over the music (2048-point frames):

- **Lane 7 was the wrong reference for stereo.** Both MT8163 Echos play only
  the right channel through the speaker, and lane 8 carries it. On stereo
  music the best fixed linear filter from lane 7 removes 0.8 dB (Echo Dot
  (2nd gen)) and 0.9 dB (Echo (2nd gen)), from lane 8 12.5 and 13.3 dB. The
  adaptive canceller, which follows the path as the music changes, removed
  more over the whole takes (ADR 0004). The spoken
  replies measured above are mono, so lane 7 served them. pinnad now also
  plays a mono mix (ADR 0004).
- **A step of 0.5 derails when someone talks over music.** On the plugin's
  own code, cold, with a talker at −45 dBFS at the microphones over the Dot's
  music, the output stayed 7–13 dB louder than the echo it should remove for
  the 30 s that followed, also over speech that it cancelled by 24–33 dB
  without the talker. A step of 0.1 held: 22–24 dB removed with lane 8, and
  the talker 33–40 dB over what was left. TECHO5 measured the same direction
  on its time-domain NLMS: 72 of 127 wake words caught over music at 0.1,
  36 at 0.5. The cost: at 0.1 the filter gains about 3 dB a second on noise,
  where 0.5 reaches 25 dB within 3 s.
- **What the linear canceller leaves can fire a word nobody said** at volume
  75 on the Echo (2nd gen), where the echo reaches −49 dBFS at the
  microphones: about one detection per 50 s take in some configurations,
  mostly Hey Jarvis, once Hey Mycroft at 1.00, with either step. WebRTC's
  suppressor never did. The cutoff and a playback ceiling wait for counted
  takes with a real talker.
- **pinnad doesn't call `Detector::set_playback`**, so the playback rule
  counted on above is off on the device. The offline takes showed no
  difference with it on; that also waits for counted takes.

## Consequences

- PipeWire runs one adaptive filter, about 19 % of a core on the Echo Dot
  (2nd gen) (1.5 ms per 8 ms of audio, measured with a test build on
  2026-11-21), and the beamformer for the direction.
- On the Echos, pinnad reads one microphone stream, `dot_wake`. The level that
  each wake word logs for ADR 0026 comes from it.
- The stream keeps whatever echo the linear filter leaves. On the Dot's
  replies the detector's VAD and threshold handled it: no false detections in
  the first measurements. At volume 75 on the Echo (2nd gen), what it leaves
  of music fired words nobody said now and then (see the amendment).
- Tests:
  - unit tests of the canceller: it converges on a known echo path, and it
    doesn't diverge under double talk;
  - a check in `just test-device` that `dot_wake` is at least 10 dB under the
    raw microphones' mean while music plays (written, not run on a device
    yet);
  - after the build, the same five-cue takes on the Dot at volumes 50, 60, 70
    and 80, to confirm the offline numbers.
- ADR 0004 and the website's audio pages describe the stream.
