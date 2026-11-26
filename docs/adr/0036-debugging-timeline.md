# ADR 0036: A debugging timeline of requests and the audio pipeline

Status: proposed (2026-11-26). Follows ADR 0029 (the dashboard password and
the confirmation at the device), ADR 0030 (recording the microphones) and
ADR 0017 (standalone mode's requests). Amends the rule that what was said
never reaches the dashboard (`pinnad/src/logs.rs`, `Status.last_error`): with
a press of Action, for at most an hour.

## Context

- **Requests are debugged from one log line.** In standalone mode pinnad
  sends requests to speech-to-text, a model, text-to-speech, MCP servers and
  the weather and radio tools. It also fetches media and calls the other
  members of a satellite group.
  - When a reply is slow or a service fails, the evidence is:
    - one line per request (`standalone/runlog.rs`): the time each stage took
      and how it ended;
    - the last failure, as one sentence;
    - a Prometheus histogram per stage.
  - None of them says:
    - which call was slow;
    - whether the time went to the first byte or the rest;
    - how many rounds a tool loop took;
    - what a service answered.
  - Release images have no SSH key (ADR 0015), so there is nothing else to
    look at.
- **The audio pipeline is debugged by recording and counting.** ADR 0030
  records 30 seconds after a press of Action. ADR 0033 used those recordings
  to replace the wake word's echo canceller. What is still open needs many
  takes, counted live:
  - the same five-cue takes at volumes 50 to 80 on the Echo Dot (2nd gen);
  - wake words that music fires now and then at volume 75 on the Echo
    (2nd gen).

  Counting means knowing, for each take, the score each word reached, the
  level and the gain at that moment, and what the speaker played. pinnad
  computes all of these for each 20 ms frame in `capture_loop`, logs them for
  detections only, and throws the rest away.
- **The two need one time axis.** A slow reply, or a wake word missed during a
  reply, crosses both. It involves the phase, the requests made during it, and
  what the microphones and the speaker did meanwhile.
- **Where the data is today** (read from the code, 2026-11-26):
  - **Outbound HTTP.** All of pinnad's HTTP goes through four helpers:
    - `pinna_assist::http::send` (providers);
    - `McpClient::post`;
    - `tools/http.rs::get_json` (weather, radio);
    - `media.rs::get_ok`.
  - **Group peers** are called through `group/client.rs::Client::reach`.
  - **Not pinnad's own HTTP:**
    - Update checks run in pinnactl, another process.
    - Sendspin and the ESPHome API are servers that others connect to.
  - **Secrets in requests:**
    - Provider keys travel only in headers.
    - Media URLs from Home Assistant carry tokens in their path
      (`/api/tts_proxy/…`) or their query (`authSig`).
    - MCP URLs may carry tokens too.
  - **Phase changes** have no timestamps outside the log. `WatchStatus` sends
    snapshots and can merge quick changes.
  - **Run stages.** In standalone mode, a request's stages report durations
    to an `Observer`. In Home Assistant mode, every voice event passes through
    `voice_event`.
  - **The wake word detector** runs only while no request streams the
    microphones, the wake word is on and no calibration runs.
- **Who can see what.**
  - The Logs page drops DEBUG and TRACE lines, because they carry transcripts.
    `Status.last_error` never carries what was said.
  - The dashboard has a password only if the owner sets one (ADR 0029).
    Without one, every dashboard RPC works for anyone on the LAN.
- **Resources.** The Echo Dot (2nd gen) has about 480 MB of RAM, and
  `MemoryMax=200M` limits pinnad. A Connect message holds at most 4 MiB.

## Decision

### Debugging is off until someone turns it on, for a time

- **Off by default.** `StartDebugging` turns it on for 15 minutes, 1 hour or
  8 hours, and pinnad refuses longer. `StopDebugging` turns it off at once.
- **In memory only**, like the Bluetooth pairing window. A restart turns it
  off, including the restart after an update installs. Updates don't wait
  for debugging: an 8-hour window would hold one off for a night.
- **Starting it again while it is on** replaces the time left and what is
  recorded. What was kept stays. Any dashboard that may start it may do this.
- **When it is off, nothing is recorded.** The taps below cost one relaxed
  atomic load each.
- **Every dashboard shows it.** `Status.debugging` holds the seconds left
  and, for the lanes and the text, their own seconds left. The Overview says
  that it is on, as it does for a recording.
- **Home Assistant shows it.** A read-only binary sensor, Debugging, is on
  while it runs, so a household that never opens the dashboard can see it.
  - Home Assistant can't start or stop it. There is no switch and no MCP
    tool. A diagnostic that reads what was said doesn't belong in an
    automation (ADR 0030's reason).
  - The sensor follows `AGENTS.md`'s pairing of state and entity without
    giving Home Assistant the action.

### What is recorded

| | How long, at most | Needs |
|---|---|---|
| Phases and events | 8 hours | nothing beyond the dashboard |
| Request metadata | 8 hours | nothing beyond the dashboard |
| Audio lanes | 1 hour | a dashboard password |
| What was said, and the replies | 1 hour | a password and a press of Action |

#### Phases and events

Each carries the time in Unix milliseconds, as log lines do. Recorded:

- **phase changes**, from a diff in `Brain::refresh()`;
- **mute**, as its own span;
- **runs and their milestones**, from `Brain::event()`, with the mode;
- **wake detections and near misses**, with the word, the score, the level
  and the gain;
- **end of speech**;
- **interruptions**, with the cause: the wake word, Action, Home Assistant or
  mute;
- **errors**;
- **Home Assistant's voice events**: speech-to-text, intent and
  text-to-speech, each start and end;
- **standalone stage spans**, with a start and an end: speech-to-text, each
  model round, text-to-speech and each tool call;
- **changes that explain a gap**:
  - the mode switching between Home Assistant and standalone;
  - the wake word turned on or off, or the chosen words changing;
  - a calibration starting or ending;
  - the microphone stream changing.

#### Request metadata

- **What is recorded:**
  - the kind: provider, MCP, tool, media or group;
  - the method and the address;
  - the status;
  - when it started, its first byte and its end;
  - the bytes in each direction;
  - how it failed.
- **Requests that stay open.** A request that is still open shows as open; a
  radio stream can stay open for hours.
- **Addresses are cut:**
  - The query string is dropped and shown as `?…`.
  - Media and MCP requests show the host only.
  - Headers are never recorded.
- **Group calls** show the RPC as the method and the member's address as the
  host.

#### Audio lanes

Recorded in buckets of 100 ms. They are numbers, never audio:

- the level before pinnad's gain, and the level after it;
- the gain;
- each wake word's score, with the threshold;
- whether the detector hears speech;
- the speaker's output level, after the mix and the volume.

How to read them:

- **Gaps.** The scores have a gap whenever the detector doesn't run:
  Listening, the wake word off, a calibration. All the lanes have a gap while
  muted. The events above say why.
- **Alignment.**
  - The microphone lanes are timed when pinnad receives a frame. That is
    after the capture graph and the echo canceller, a few tens of
    milliseconds after the sound.
  - The speaker lane is timed when the sound plays.
  - The offset is not measured. Like ADR 0030's channel 10, the lanes are
    not aligned to the frame.

#### What was said, and the replies

- These are the bodies of the requests: the transcript, the prompt, the
  model's reply, the text spoken, tool arguments and results.
- **Which bodies.** Only text, JSON and event streams. Never multipart or
  audio, and never a group Join request, which carries the group key.
- **Masked.** Each body has the owner's keys, and anything that looks like a
  key, replaced with `[key]`. Its spacing and line breaks are kept.
- **Capped.** At most 16 KB each way per request: the first and the last
  8 KB, with `…` between them. A model request starts with the instructions
  and the tools, and ends with the newest turn, so both ends matter.

### Why the lanes need a password, and the text a press

- **Phases, wake events and request metadata.** These are already in the log,
  or on the Assistant pages. Anyone who can use the dashboard can see them
  today.
- **The lanes** show, every 100 ms, when someone in the room speaks. They
  don't carry what was said.
  - They are recorded only while a password is set, so only someone signed
    in can turn them on and read them.
  - They stop after an hour, so they don't become a record of who was home.
- **The text is what was said.** Until now it never reached the dashboard. It
  gets ADR 0030's safeguards for audio:
  - **A confirmation at the device.** `StartConfirmation` takes a new
    purpose, `DEBUGGING_TEXT`. Its rules are ADR 0030's for a recording:
    - a grant bound to the session that asked;
    - good for 10 seconds;
    - the one-at-a-time rule and the cooldown.
  - **The ring.** It shows the recording confirmation's pattern, because both
    approve the same thing: what you say goes to this browser.
  - **A time limit**: an hour at most.
- **Mute is the stop in the room.** Pressing mute stops the lanes and the text
  for the rest of the window; the rest goes on. This matches ADR 0030, where
  mute ends a recording.
- **The ring doesn't show debugging.** No audio leaves the device, the lanes
  stop within an hour, and the text started with a press at the ring. The
  Home Assistant sensor and every dashboard show it instead.
- **When sessions end, debugging ends.** A new password, removing it, a reset
  by a press, or Sign out everywhere stops debugging and clears everything
  recorded. The next password holder doesn't inherit it.

### Kept for 10 minutes, in memory

- **The 10-minute limit.** Nothing older than 10 minutes is kept, on the
  satellite or in the page. That covers "it just missed the wake word": open
  the page afterwards, and it is there.
  - Age and order use the monotonic clock. Unix time is stamped when an entry
    is sent, so a clock set late (no network time yet) doesn't throw entries
    away.
  - A request that is still open is kept until it ends, and shown clipped.
- **Stop** clears everything at once, on the satellite and in every open
  page. **Download** first to keep it.
- **When the time runs out**, recording stops, and what was kept ages out
  over the next 10 minutes.
- **Budgets.** The oldest entries go first when a budget is full:

  | What | At most |
  |---|---|
  | Events | 5,000 |
  | Requests | 1,000 |
  | Lanes (10 minutes) | about 250 KB |
  | Text | 2 MB |

  Nothing is written to storage.

### How the dashboard receives it

- **`WatchTimeline` is a server stream on `DeviceService`.** It sends:
  1. the state: on or off, until when, what is recorded and for how long,
     the bucket length, and the wake words' names;
  2. what was kept, in messages of at most 1 MiB;
  3. what happens next.
- A request is sent when it starts and again when it ends.
- While debugging is off, the stream sends the state and waits.
- A browser that falls behind is cut off. The dashboard connects again and
  gets what was kept, as the Logs page does.

### How requests are recorded

- **A traced send.** `pinna-http` gets `send(kind, request)`. Its response
  body:
  - counts the bytes;
  - notes the first byte and the end;
  - notes a stop when the caller drops it.

  Each request is reported to one sink that pinnad installs.
- **Who uses it:** the four helpers and the group client.
- **Not recorded:**
  - update checks, which run in pinnactl;
  - Sendspin and AirPlay, because others connect to the satellite;
  - the ESPHome API itself. Its voice events are recorded instead.
- **Home Assistant mode.** Home Assistant makes the requests itself, and the
  satellite only fetches the reply's audio. The page says so where the
  requests would be.

### In the dashboard

- **System > Timeline** is a subpage that fills the window, as Logs does.
- **Off.** Choose how long, then **Start**. Two choices go with it:
  - **Audio levels.** Shown only with a password; up to an hour.
  - **Include what was said and the replies.** Shown only with a password;
    up to an hour. It waits for a press of Action, as Record audio does.

  Without a password, the page says: "Set a dashboard password to record the
  audio levels or what was said." with a link.
- **On.** The page shows:
  - the time left;
  - **Stop**;
  - **Live** or **Pause**;
  - a window of 1, 5 or 10 minutes;
  - two views of the same data:
    - **Timeline**: the phases as coloured sections, the events, the lanes
      and the requests as bars on one time axis;
    - **Requests**: a table.

  Selecting a request or an event shows its details, with any text folded.
- **Download** saves what the page holds as JSON. When it holds text, a line
  says that the file contains what was said.
- **Drawing.**
  - Hand-written SVG. The dashboard has no chart library and doesn't take
    one.
  - The phases use the design system's phase colours.
  - The lanes get chart tokens of their own in `design/tokens.json`: series
    colours, the threshold and grid lines.
- **System > Record audio stays as it is.** Recording from the timeline is
  left for later.

## Consequences

- New:
  - `StartDebugging`, `StopDebugging` and `WatchTimeline`;
  - the `DEBUGGING_TEXT` confirmation purpose;
  - `Status.debugging`;
  - the Debugging binary sensor;
  - a traced send in `pinna-http`;
  - taps in `capture_loop`, the speaker and the Brain;
  - a read-only score accessor in `pinna-wakeword`;
  - chart tokens;
  - the Timeline page.
- **The voice flow is unchanged with debugging on.** `just e2e` runs a
  request with it on and checks what it recorded.
- **The speaker** computes an output level for each 20 ms chunk while the
  lanes are on. `just sendspin-conformance` covers the timed source that
  shares that path.
- **Memory.** pinnad's memory grows by at most about 3 MB while debugging
  runs.
- **Docs.** The dashboard, troubleshooting and entities pages describe the
  timeline.

## Alternatives not taken

- **Two pages**, one for requests and one for audio. A problem that crosses
  both would need two clocks lined up by hand.
- **One trace per request** (wake, stages, calls) with no continuous lanes. It
  misses near misses, background requests and the minutes around a missed
  wake word.
- **A log line per request.** It is cheaper, but it has no lanes or text and
  no shared axis, and it fills the journal that Copy for an issue sends.
- **Always on, or lanes for 8 hours.** That would keep a record of when people
  speak at home, for nobody in particular.
- **The text with a password alone.** A password holder could read what the
  household asks, with nobody in the room knowing.
- **Audio in the timeline.** Audio leaves the device only through ADR 0030's
  confirmation, and the timeline records none.
- **A `tracing` layer for requests.** pinnad has no spans to hang it on, and a
  request's end is known only where its body is read.
- **Prometheus metrics** (ADR 0020). A scrape every 15 seconds can't show a
  20 ms frame or a single request.
- **Keeping it on storage across restarts.** Data at rest needs a cleanup to
  get right. A restart during debugging is rare, and starting again is cheap.
