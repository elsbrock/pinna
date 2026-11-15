# ADR 0004: The Echo Dot's microphone path runs in PipeWire; pinnad keeps the speaker

Status: accepted (2026-11-15). Supersedes the "audio goes straight to ALSA" part
of ADR 0002.

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
(`pinna-pipewire.service`, `pinna-wireplumber.service`, only on MT8163), with a
static microphone graph (`/etc/pipewire/pipewire.conf.d/10-echo-dot.conf`, no
device monitors) at the microphone's native 16 kHz:

```
dot_mics (hw:0,24, graph clock, IRQ mode)
  -> filter-chain: LADSPA pinna_beam (calibration, gain, beamformer) -> dot_beam (mono)
  -> module-echo-cancel (WebRTC, mono) -> dot_mic_aec -> pinnad (wake word, Home Assistant)
pinnad -> dot_ref (16 kHz mono copy of everything it plays) = echo reference
```

The speaker is not in PipeWire. pinnad drives it through ALSA at 48 kHz
(`EchoDotPipeWire` backend: aplay, its amp/codec sequences around playback,
and the speaker EQ — a 150 Hz high-pass), and writes a 16 kHz mono copy of
every chunk it plays into `dot_ref` through a bounded queue (dropped, never
blocking, if PipeWire lags). The two clocks drift apart (speaker ≈ +320 ppm,
microphones ≈ +5 ppm against the CPU), so pinnad fits both from the PCMs'
hardware pointers (`/proc/asound/card0/pcm*/sub0/status`), paces the speaker at
its measured rate and resamples the reference from the speaker clock to the
microphone clock. WebRTC's delay estimator only absorbs the fixed offset
between the two paths.

- **Beamform first, then mono echo cancellation**: the usual design for small
  arrays and far cheaper than per-microphone AEC. The beamformer is LibreEcho's
  superdirective fixed-beam design, ported to Rust (within 1 LSB of the C on
  ≥ 99.9 % of samples) and loaded into
  PipeWire as a LADSPA plugin (`crates/pinna-ladspa`).
- **The speaker stays out of PipeWire**: full 48 kHz playback, and the amp is
  only ever switched by the code that knows when it plays — no PipeWire graph
  restart can reach the speaker.

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
- Laptops keep using the plain `PipeWire` backend; the VM images run without
  an audio backend.
- The beamformer runs in PipeWire's process, so pinnad cannot ask it which
  beam it follows. After every cycle the plugin writes a seqlock snapshot
  (selected beam, per-beam power) to `/run/pipewire/pinna-beam`
  (`pinna_audio::beamview`), and pinnad maps it read-only to point the
  listening ring at the talker. The file only ever grows, so a reader's mapping
  stays valid when PipeWire restarts. A snapshot older than 500 ms means the
  graph has stopped. The ring shows the plain listening pulse until a
  confident snapshot arrives, and again after a second without one.
