# ADR 0030: Recording the microphones from the dashboard, confirmed at the device

Status: proposed (2026-11-20), accepted (2026-11-21), built (2026-11-21),
with the tests that run without hardware; the virtual Echo Dot recorded the
first file. The debugging file and the ring pattern haven't run on an Echo.
Follows ADR 0029 (the confirmation at the device), ADR 0004 (the audio graph)
and ADR 0005 (the dashboard).

## Context

- The audio pipeline is debugged by ear: the wake word, beamforming, echo
  cancellation and pinnad's automatic gain. One problem is open: on the
  Echo Dot (2nd gen) at reply volume 70 (standalone mode, 2026-11-18), the
  wake word said during a reply didn't interrupt it, and pinnad logged not
  even a near miss. The suspects
  are residual echo at the start of a reply and the gain dipping during it.
  Telling them apart takes a recording of the echo canceller's output, before
  and after the gain, during a reply while someone says the wake word.
- Today that takes `pw-record` over SSH or a serial console. Release images
  carry no SSH key (ADR 0015), and owners manage the satellite from the
  dashboard. An owner who reports an audio problem can't send a recording.
- The first goal says that audio leaves the device only after the wake word
  or when someone starts a request on purpose, and that the ring shows
  whenever the device is listening. A recording that a browser downloads is
  audio leaving the device, so it needs the same guarantees.
- The capture graph on the Echo Dot (2nd gen) (ADR 0004,
  `mkosi/mkosi.extra/etc/pipewire/pipewire.conf.d/10-echo-dot.conf`):
  `dot_mics` (9 lanes, S24LE, 16 kHz: 7 microphones and 2 reference lanes,
  `pinna-audio`'s `dsp.rs`) → the `pinna_beam` LADSPA beamformer → mono
  `dot_beam` → WebRTC echo cancellation, whose reference is `dot_ref`, the
  16 kHz copy of everything pinnad plays → `dot_mic_aec`. pinnad records
  `dot_mic_aec` as mono s16 at 16 kHz, applies its automatic gain, and hands
  the same 20 ms frames to the wake word and to the request
  (`pinnad/src/main.rs`, `capture_loop`). pinnad never sees the raw lanes on
  this graph. Whether the Echo (2nd gen) captures through this graph with
  the same lanes is not checked on a device here.
- Other backends: `echodot` (ALSA, no PipeWire) is a fallback that mixes the
  raw lanes down without beamforming or echo cancellation; `pipewire` (the
  virtual Echo Dot, a PC) records mono; `wav` reads a file. The microphones
  run for the life of pinnad; only mute drops frames.
- ADR 0029's confirmation: the ring shows the confirm pattern for 60
  seconds, a press of Action hands the browser that asked a one-time grant,
  only one waits at a time, a new one can start a minute after the last
  ended (10 seconds after a cancel), and signed-in dashboards show who is
  waiting and can stop it. A grant is a string checked on its own
  (`pinnad/src/auth/confirm.rs`), and `WatchConfirmation` needs no session.
- Action is also push-to-talk. A press that confirms the wrong thing looks,
  to the person pressing it, like a request starting.
- The phase is read by Home Assistant, by MCP agents (`pinnad/src/tools/device.rs`) and
  by the dashboard; automations act on Listening.
- The Echo Dot (2nd gen) has about 480 MB of RAM (ADR 0005). The Connect
  service has connectrpc's default limits (4 MiB per message); the dashboard
  already reads server streams (`WatchLogs`, `WatchRing`).

## Decision

### Who it is for

- **Debugging the audio pipeline**: by developers, and by owners whom a
  developer asks for a recording. Not an everyday control.
- **It needs a dashboard password**. While the dashboard is
  open, anyone on the LAN could start a confirmation, and an owner pressing
  Action to talk would hand them the room's audio. With a password, only
  someone signed in can ask, and the press is still needed.

### What is recorded

- **Up to 30 seconds.** The dashboard records for 30 seconds, and **Stop**
  ends it sooner. pinnad refuses more than 30.
- **What the satellite hears**, on every backend: the frames that the wake
  word and the request get, after beamforming, echo cancellation and
  pinnad's gain, tapped in `capture_loop` after the gain. WAV, 16-bit PCM,
  mono, 16 kHz; 30 seconds is 0.96 MB. It plays anywhere.
- **Everything for debugging**, on the Echo's PipeWire graph only: one WAV
  (`WAVE_FORMAT_EXTENSIBLE`), 24-bit PCM, 16 kHz, 11 channels:
  1. to 9. the lanes of `dot_mics` as the codec delivers them, nothing
     mixed, filtered or gained: 7 microphones, then the 2 reference lanes;
  10. the echo canceller's output before pinnad's gain;
  11. the echo reference as the echo canceller gets it.

  30 seconds is 15.84 MB. The lanes and the reference come from two
  `pw-record` streams that run only during the recording: one on
  `dot_mics`, one on the monitor of `dot_ref_sink`, which the echo
  canceller's playback feeds. Channel 10 comes from pinnad's own stream. The
  other backends have nothing this file would add.
- **The channels are not aligned to the sample.** The streams start within
  a few tens of milliseconds of each other, and the beamformer and echo
  cancellation add their own delay. The dashboard says so; a
  cross-correlation finds the offsets.
- File names: `<satellite name>-heard-<date>-<time>.wav` and
  `<satellite name>-debug-<date>-<time>.wav`.

### Confirmed at the device

- **Each recording waits for a press of Action, through ADR 0029's
  confirmation**: `StartConfirmation` takes a purpose (`PASSWORD`, the
  default and today's meaning, or `RECORDING`). The one-at-a-time rule, the
  cooldowns, **Cancel** and **Stop it** are the confirmation's.
- **The ring shows its own pattern for a recording**, the confirm
  pattern's circling light in the Listening colour, so the person at the
  device can tell it from a password confirmation. `pinna-hal`'s animations
  and `design/ring.ts` change together.
- **A recording grant is bound to the session that asked**, and is good
  for 10 seconds, not 5 minutes: the recording follows the
  press. A grant presented from another session is refused.
- `RecordAudio` takes the grant and starts at once. It is refused, with
  the reason, if the microphones are muted or a calibration runs when it is
  called; a refusal leaves the grant unused.
- Signed-in dashboards show a waiting confirmation with its purpose ("A
  browser at 192.168.1.23 is waiting for a press of Action to record from
  the microphones. Don't press it unless that was you."), and a running
  recording with the address, the seconds left and **Stop it**.

### On the satellite while it records

- **It starts whatever the satellite is doing**, except while muted or
  while the wake word calibration runs.
- **The satellite works as usual during a recording**: the wake word,
  requests, replies and interrupting them, timers and their alarms,
  announcements, group messages and music. The recording captures them as
  they happen; a reply interrupted by the wake word is what it is for. The
  recording itself ducks nothing.
- **The ring shows Listening for the whole recording**, above a running
  timer's arc, a notification, the ring light and the music, so that it
  always says the microphones record. A request, a ringing alarm, mute, a
  confirmation, calibration and Bluetooth pairing keep their own patterns,
  which also say that the satellite is active.
- **The phase doesn't change.** A recording is its own field in `Status`
  (the seconds left, the address that asked), so Home Assistant, agents and
  automations that act on Listening aren't misled. The Overview shows it.
- **What ends it**: its length; **Stop** in the dashboard that recorded it
  or **Stop it** in another signed-in one; mute, at once, which is the stop
  for whoever is in the room; the stream breaking; pinnad stopping; leaving
  the page or closing the tab, which also loses what arrived. Action keeps
  its usual job during a recording. Otherwise the browser keeps what arrived
  and says why the recording ended.
- An update's restart waits for a recording to end, as it waits for Idle.
- pinnad logs the start, the end, why it ended, and the address.

### How the browser receives it

- **`RecordAudio` is a server stream on `DeviceService`.** Its first
  message gives the rate, the length, whether the debugging sources follow
  and how many lanes they have. Then each source comes in its own chunks of
  about 100 ms as `bytes`: what the satellite hears, the signal before the
  gain and the reference as 16-bit mono (3.2 KB), the lanes as 24-bit
  interleaved (43.2 KB on the Echo Dot (2nd gen)). The last message says how
  it ended, with how many seconds the first file holds. The dashboard puts
  the debugging sources together into the 11-channel file, writes the WAV
  headers and offers the files to save.
- **Nothing is stored on the satellite.** The chunks go from the taps to
  the stream through a bounded queue of 2 seconds (about 1.1 MB). A browser
  that falls further behind ends the recording ("The browser fell behind:
  the files hold 12 s."); pinnad never buffers more. The two `pw-record`
  processes live only for the recording.
- The browser holds the files in memory, at most 17 MB.

### Not in Home Assistant, not for agents

- No entity and no MCP tool. A recording is a diagnostic whose result is
  files in the browser that asked; an ESPHome entity can't carry them, and a
  button there would let an automation start one, against the first goal.
  `AGENTS.md` pairs each owner-facing setting with an entity; a recording is
  an action, not a setting, and it makes sense only in the dashboard.

### In the dashboard

- **A subpage, System > Record audio**: it has a flow of its
  own (a confirmation at the satellite, a wait), so it gets a page. While
  there is no password, it says that recording needs one and links to
  setting it. Otherwise: **Record**, the confirmation, a countdown with
  **Stop**, then the files to save, with their size and length, a line on
  the alignment, and a line that the files are the room's audio: send them
  privately, not in a public issue.

## Consequences

- The dashboard page and the troubleshooting page change with the code.
- New: a purpose on `StartConfirmation`, a purpose and a running recording
  in `GetSecurity` and `Status`, `RecordAudio`, a ring pattern, taps and a
  second capture stream in pinnad, a WAV writer in the dashboard with unit
  tests. The voice flow carries on during a recording, so `just e2e` runs
  with a recording across a request and an interrupted reply.
- A confirmed recording starts the confirmation's one-minute cooldown: two
  recordings in a row are a minute apart.
- The debugging file exists only on the Echo's PipeWire graph; on the
  virtual Echo Dot only the first file can be tested. On the Echo (2nd gen)
  its lanes are whatever its capture node delivers, documented once
  measured on a device.
- Recording raises pinnad's memory by at most the 2-second queue, and adds
  the two `pw-record` processes while it runs.

## Alternatives not taken

- **A quiet recording, in which no request starts and anything that talks
  ends it.** Simpler to reason about, but it can't capture a reply and its
  interruption, the open problem this is for.
- **Recording without a password, the press alone.** Action is
  push-to-talk; a habitual press would hand the room's audio to anyone on
  the LAN, and nobody would notice.
- **SSH keys from the dashboard** (ADR 0010, possible since ADR 0029). They
  would let a developer use `pw-record` on their own satellite, but not let
  an owner send a recording.
- **A file on the satellite, downloaded afterwards.** Audio at rest on the
  device, in RAM on tmpfs (17 MB), with a cleanup to get right; the stream
  needs neither.
- **A WebSocket, like the virtual device's `/virtual-device`.** A second
  transport with its own sign-in check; the Connect stream already has the
  session.
- **Compressed files** (Opus, FLAC). Opus hides the artefacts that a
  recording is for; FLAC needs an encoder on the device. 17 MB at most
  doesn't need it.
- **A raw file on every backend.** On `pipewire` it repeats the mono
  stream; `echodot` is a fallback without the processing to debug.
- **Longer recordings.** 30 seconds covers a wake word, a request and an
  interrupted reply; longer ones invite using the satellite as a recorder.
