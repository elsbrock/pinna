# ADR 0004: Audio runs through a system-wide PipeWire graph on the Echo Dot

Status: accepted (2026-11-15). Supersedes the "audio goes straight to ALSA" part
of ADR 0002.

## Context

The wake word has to keep working while the Dot talks or plays music, which
needs echo cancellation with the speaker signal as reference. The Dot's audio
hardware is two unrelated clock domains: the speaker is an I2S codec (AFE DL1,
48 kHz only, read/write access only, 128 ms buffer) and the seven microphones
come in over SPI from an FPGA (9 lanes, 16 kHz, packed 24-bit, bursty pointer,
period 640, buffer ≤ 2560 frames).

## Decision

PipeWire and WirePlumber run as system services on the device
(`pinna-pipewire.service`, `pinna-wireplumber.service`, only on MT8163). The
graph is static (`/etc/pipewire/pipewire.conf.d/10-echo-dot.conf`, no device
monitors):

```
dot_mics (hw:0,24, graph clock, IRQ mode)
  -> filter-chain @16 kHz: LADSPA pinna_beam (calibration, gain, beamformer) -> dot_beam (mono)
  -> module-echo-cancel (WebRTC, mono, reference = dot_out) -> dot_mic_aec -> pinnad
pinnad -> dot_out -> filter-chain speaker EQ (dot_eq) -> dot_speaker (hw:0,23, follower)
```

- **The microphone is the graph clock.** It always runs (wake word) and its SPI
  driver only behaves IRQ-driven; as a rate-matched follower it re-prepared in a
  loop and took the speaker with it (audible clicks). The speaker follows with
  adaptive resampling and never suspends, so the codec is not restarted.
- **Beamform first, then mono echo cancellation.** Debian's
  webrtc-audio-processing 1.x as used by PipeWire's AEC plugin processes one
  capture channel; beamforming before it is the usual design for small arrays
  and is far cheaper than per-microphone AEC. The beamformer is LibreEcho's
  superdirective fixed-beam design, ported to Rust (bit-exact) and loaded into
  PipeWire as a LADSPA plugin (`crates/pinna-ladspa`), so pinnad reads one
  processed stream.
- **pinnad keeps the amp.** PipeWire never touches the codec/amp mixer controls;
  pinnad's `EchoDotPipeWire` backend still runs the arm/unmute/disable sequences
  around its own playback and the microphone PGA setup.

Measured on the device (music at moderate volume, speech band 300–3400 Hz):
echo at the microphone −60 dB → −97 dB after cancellation; the graph runs with
no underruns; PipeWire uses ~40% of one core with AEC active.

## Consequences

- pinnad falls back to driving ALSA directly (`EchoDot` backend) when the
  PipeWire socket is absent, so the device keeps working if the graph fails.
- Changing the audio path is configuration (and the LADSPA plugin), not pinnad
  code; laptops/VMs keep using the plain `PipeWire` backend.
- The speaker EQ is a 150 Hz high-pass only until the response is measured.
