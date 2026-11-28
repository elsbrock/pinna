# ADR 0038: Recognizing who speaks, on the satellite

Status: proposed (2026-11-27), accepted (2026-11-27), built for release 1
(2026-11-28), and tried on the owner's Echo Dot (2nd gen) with two people.
The model choice holds only if the owner's recordings pass the test under
"Open before building", which is still open; until then the thresholds are
provisional. Amends ADR 0017
(standalone mode: the prompt, the tools, the conversation, typed questions) and
ADR 0036 (the debugging timeline). Touches ADR 0007 (an entity), ADR 0013 (wake
words, later), ADR 0029 and ADR 0030 (a confirmation at the device for
enrollment).

## Context

- **What the owner asked for.** A satellite should know which person in the
  household is speaking:
  - In standalone mode, each person gets their own extra instructions and,
    later, their own tool permissions.
  - In both modes, Home Assistant gets a "Last speaker" entity.
  - The ESPHome voice protocol has no speaker field, so Home Assistant's own
    conversation agent can't be told who spoke. The entity is all Home Assistant
    gets.
- **Voice identifies; it doesn't authenticate.** A recording fools it, and
  family voices get mixed up. A child taken for an adult gets the adult's
  settings.
- **A spike measured the candidates** (`build/speaker-spike`, not committed):
  - Two families were tried, with sherpa-onnx's ONNX exports and our own:
    - 3D-Speaker's CAM++. "Advanced" is the version trained on Chinese and
      English speech: Apache 2.0, a 192-number voiceprint, 28 MB at 32-bit.
    - ReDimNet2 from PalabraAI (MIT), sizes B0 to B3.
  - Both need a front end that turns audio into features. The spike wrote both
    in Rust:
    - CAM++ uses Kaldi's 80-bin filter bank. With tract it matches sherpa-onnx
      at cosine 1.000000, on x86 and on armv7 under qemu.
    - ReDimNet2 uses its own 72-bin front end, which matches PyTorch.
  - Time on the Echo Dot (2nd gen) on 2026-11-27, with tract 0.23.10, one
    thread, and pinnad running alongside:

    | Model | 1.5 s of speech | 3 s of speech | Peak memory |
    |---|---|---|---|
    | CAM++ advanced | 764 ms | 1504 ms | 71 MB |
    | ReDimNet2-B1 | 1458 ms | 3007 ms | 41 MB |
    | ReDimNet2-B2 | 1920 ms | — | 50 MB |
    | ReDimNet2-B3 | 4260 ms | — | 54 MB |

    Preparing a model takes 5–9 s, once per input length.
  - **Why ReDimNet2 is slow on the satellites:**
    - Half of B1's time goes to depthwise convolutions: 690 ms on the Echo Dot
      (2nd gen) against 5 ms on a laptop.
    - tract-linalg 0.23 has kernels for them on x86_64, arm64 and WebAssembly,
      but not on the 32-bit ARM the satellites run.
    - Even with such a kernel, B1 would only match CAM++.
  - **tract 0.22.5,** which Pinna uses today, runs CAM++ at about half speed on
    the satellites unless it is patched (eager im2col on small kernels): 2 s of
    speech took 1358 ms unpatched against 1040 ms patched. tract 0.23 needs no
    patch, and costs about 2 MB more in the image.
- **Accuracy in the home isn't measured yet.** Published results use clean
  clips of about 8 s. A satellite hears short commands from across a room,
  with music or a TV. The owner records the test set through ADR 0030's page,
  using the source "What the satellite hears", so the recordings are the
  signal requests get.
- **The budgets:**
  - pinnad runs at 46–58 MB under `MemoryMax=200M`, and about 0.72 cores at
    idle.
  - The endpointer waits 800 ms of silence before it ends a request
    (`pinna-vad`, `EndpointConfig`).
  - In standalone mode, speech-to-text starts once the request has ended. A
    fast service answers within a few hundred milliseconds after that.
  - The Echo Dot (5th gen)'s system slot holds 131,088,384 bytes. The
    0.7.0-beta.2 root image is 104,919,040 bytes. Leaving out unused Debian
    parts brought it to 99,405,824 bytes on main (2026-11-28), 31.7 MB free.
- **The Echo Dot (3rd gen)**, being ported, has four Cortex-A35 cores, which
  are slower than the Echo Dot (2nd gen)'s Cortex-A53s. It isn't measured.
- **Hosted speaker recognition was ruled out:**
  - Azure's was retired on 2025-09-30.
  - pyannoteAI works on uploaded jobs.
  - Picovoice Eagle is proprietary.
  - All of them would send voices off the device, against the first goal.
- **A product review** (2026-11-27) found these gaps in the first draft:
  - The test had no voices that aren't enrolled.
  - Short commands raced speech-to-text.
  - Short follow-ups reset the conversation.
  - Limits set on a person fail open whenever recognition fails.
  - The sensor showed the previous speaker while a new request ran.

  This version answers each of them.

## Decision

### In three releases

1. **Recognition:** the People page, enrollment, "Last speaker", each person's
   extra instructions (set by the owner), the conversation rule, the timeline
   and the metrics.
2. **Tools per person**, once the metrics show how often recognition falls
   back and mixes people up at home.
3. **Notes a person keeps by voice**, with a decision of their own (below).

The tract upgrade lands before release 1, on its own, with the wake word's
parity test.

### Off until someone enrolls

- Recognition is opt-in. With nobody enrolled, pinnad doesn't load the model.
  In standalone mode, spoken requests get Everyone else's extra instructions;
  nothing else changes.
- pinnad loads the model in the background when an enrollment starts, so the
  load overlaps the recording, and keeps it loaded, including at each start,
  while someone has a voiceprint from the installed model. Loading takes
  about 13 s on the Echo Dot (2nd gen) (2026-11-28). Until the model is
  ready, requests run as "Everyone else".
- When nobody has a usable voiceprint any more, pinnad unloads the model.

### People, and Everyone else

- **A person** has:
  - a name;
  - extra instructions, set by the owner on the dashboard, at most 1,000
    characters;
  - from release 2, a tool choice: either "all tools the owner switched on",
    where new tools join automatically, or "only these", where new tools stay
    off;
  - a voiceprint, which belongs to this satellite.

  There is no gender, age or other inferred field. Pronouns and how to address
  someone go in their extra instructions.
- **At most 10 people.** Past that, mix-ups grow, and the dashboard can't
  warn about every close pair.
- **"Everyone else"** is a built-in profile with the same extra instructions
  and, from release 2, the same tool choice. It can't be removed. It applies to
  a spoken request when:
  - nobody is enrolled;
  - nobody matches closely enough;
  - the model isn't loaded yet;
  - recognition isn't finished when speech-to-text returns;
  - there is too little speech to recognize.
- **Limits belong on Everyone else.** Recognition fails whenever someone
  speaks briefly, from far away, over music, or before the model has loaded.
  A limit set only on a person is then lost. So people widen what Everyone
  else may do; they don't narrow it.
  - The People page says this.
  - From release 2, it warns when a person's tools are narrower than
    Everyone else's.
- **Data stays open for later.** A person record can gain wake word ids, to
  bind a wake word to a person or to a mode (ADR 0013). That isn't built now.

### How a request is recognized

- **Recognition uses only the request's own audio**, after the wake word: the
  frames pinnad already hands to the request, after the echo canceller and the
  gain. It adds no new audio capture.
- **It runs as early as it can.** A thread of its own, at a lower priority
  than audio, computes a voiceprint from 1.5 s of speech.
  - **First run:** once there is 0.5 s of speech, at the first pause, or at
    1.5 s of speech, whichever comes first.
  - **Second run:** if the first had less than 1.5 s of speech and the person
    goes on speaking, one more run uses 1.5 s.
  - Speech shorter than 1.5 s is repeated to fill it.
  - The request takes the newest confident match that finished in time. A
    later uncertain run doesn't override it.
  - A pause gives the run the endpointer's 800 ms silence wait, plus the time
    speech-to-text takes, before anyone needs the result.
  - One input length means one prepared plan, which keeps memory bounded.
- **The request never waits for recognition.** Speech-to-text runs as today.
  If no result is ready when speech-to-text returns, the request runs as
  Everyone else, and the log and the timeline say so.
- **Matching:**
  - The voiceprint is compared with each enrolled one by cosine similarity.
  - The best match wins only if it clears a threshold and leads the second-best
    by a margin. Otherwise the result is uncertain.
  - The recordings set both values. The goal is at most 1% of requests taken
    for the wrong person.
  - Until then, the threshold is 0.4 and the margin 0.1. On the Echo Dot (2nd
    gen), two people and about 20 commands of 1.1 to 1.5 s of speech scored
    0.49 to 0.68 against the speaker's own voiceprint and at most 0.34
    against the other's (2026-11-28). At 0.5, a third of the commands fell
    back to Everyone else.
- **In Home Assistant mode,** the same recognition runs on the audio that
  streams to Home Assistant. It only sets "Last speaker".

### Who the conversation belongs to

- **Only a confident match to a different person is a speaker change.**
  - An uncertain result is never a change. On a follow-up (the 4 s after a
    reply, without a wake word), the conversation's speaker carries over:
    their instructions apply, and the conversation goes on. A short "Yes" or
    "The kitchen" doesn't reset anything.
  - On a new request with the wake word, an uncertain result runs with
    Everyone else's instructions. The conversation goes on.
- **When someone else speaks within the 5-minute window**, the owner chooses
  what happens:
  - **Start a new conversation**, the default: the new speaker doesn't get the
    earlier turns.
  - **Keep the conversation**: everyone shares it.

  Either way, the instructions follow the current speaker.

### Standalone mode: prompt and typed questions (amends ADR 0017)

- **The prompt** is, in order:
  1. the owner's prompt, or the voice rules;
  2. the extra instructions of the person or of Everyone else;
  3. the context.

  When the profile is a person's, recognized or carried over on a follow-up,
  the context names them ("The speaker is Anna."). For Everyone else, it
  names nobody.
- **Typed questions run with the owner's settings**: the owner's prompt and
  switches, with no person and no extra instructions. Anyone who can type on
  the dashboard can already change every setting there, so limiting them
  would protect nothing. They don't change "Last speaker".
- **From release 2, a tool is offered only when two things are true:** the
  owner's switch is on, and the current profile's tool choice allows it.
  pinnad checks this twice: when it builds the tool list, and again when the
  model calls a tool.
- **Agent bridges (Hermes, OpenClaw) get the person only through the
  prompt** pinnad builds: the extra instructions and the speaker's name. How
  they handle users is likely to change, so pinnad keeps one user per
  satellite for them. An agent that keeps its own session (OpenClaw) keeps it
  across a speaker change.

### Notes kept by voice: release 3, decided separately

- A recognized person could ask the satellite to remember things about them,
  in notes kept apart from the owner's instructions.
- It needs its own decision first:
  - It reverses ADR 0017's "pinnad keeps no memory".
  - Text from a tool's result could write into the notes. That's why ADR
    0017 starts `install_update`'s switch off, and the notes tool would start
    off the same way.
  - A mix-up lets one person write another's notes.

### Home Assistant: "Last speaker"

- **A text sensor, "Last speaker"** (`last_speaker`), next to "Last wake
  word". Its state:
  - **Empty** from the start of each request: the wake word, Action, or a
    request from Home Assistant.
  - **The person's name** once recognition is confident.
  - **`Everyone else`** once it isn't, including too late or too short.
  - In standalone mode, it shows the profile the request ran with, once the
    request has it: a speaker carried over on a follow-up shows their name,
    and a later result doesn't change it.

  Every request clears it first. An automation that waits for a
  name sees repeats by the same person, and nothing reads the previous
  speaker by mistake.
- **It's usually set before Home Assistant has the text,** so a sentence
  trigger or an agent's prompt template can read it. An automation that runs
  while it's still empty should treat that as unknown.
- **Renaming a person changes the state.** The People page warns that
  automations comparing the name need updating.
- **Home Assistant keeps a history of who spoke to which satellite, and
  when.** The People page says so, beside the note that voiceprints stay on
  the satellite.
- Typed questions don't change it. It's state, not a setting: people are
  managed only on the dashboard, because enrollment needs a press of Action
  at the satellite.

### Enrollment

- **On the dashboard, Voice & sound > People > a person > Enroll voice.** The person stands at
  the satellite. The owner starts enrollment on the dashboard, and the person
  presses Action.
  - The press uses ADR 0029's confirmation with a new purpose, `ENROLL`. It
    follows that confirmation's rules: one at a time, the cooldowns and
    **Cancel**. The ring shows a recording's confirm pattern, as for the
    debugging timeline's text (ADR 0036): both let the microphones record.
  - It doesn't need a dashboard password: no audio leaves the satellite, only
    a voiceprint is stored on it. If there is a password, managing people
    needs a sign-in, like every other setting.
- **The person speaks for about 30 seconds.** They read a text the dashboard
  shows, or talk freely about anything, which works for children who can't
  read yet.
  - Nobody else should talk, and a TV or radio should be off. The page says
    so, because a steady second voice can end up in the voiceprint.
  - The ring shows Listening throughout.
  - The dashboard shows the level and how much speech it has.
- **The satellite stays quiet while it enrolls.**
  - It doesn't start while music, a reply, an announcement or a ringing alarm
    plays.
  - The wake word is paused.
  - An announcement or an alarm that arrives ends enrollment; it plays, and
    nothing is stored.
- **The voiceprint:** the satellite takes the frames requests get, cuts them
  into 1.5 s windows of speech, and stores the normalized mean of their
  voiceprints. No audio is stored, and none goes to the browser.
- **Checks before anything is stored:**
  - at least 10 s of speech;
  - not too quiet, not clipped;
  - windows that agree with each other.

  A failed check says why and what to do: "Too quiet. Stand closer, within 1
  m." Muting, a press of Action, **Cancel**, leaving the page or a broken
  stream end it and store nothing.
- **Enrolling again** replaces the voiceprint only once the new one passes the
  checks. A failed attempt keeps the old one.
- **A close pair gets a warning.** If a new voiceprint is close to another
  person's, the dashboard says the satellite may mix them up, and suggests
  enrolling again closer to the satellite.
- **Each person enrolls on each satellite.** Each Echo model's microphones and
  echo canceller color the voice. Sharing voiceprints across satellites waits
  for a measurement with Echo Dot (5th gen) recordings.

### Storing voiceprints

- **The people list** (names, extra instructions, later tool choices) is in its
  own file under `/var/lib/pinna/`, separate from `standalone.toml`, because
  Home Assistant mode uses it too. Edits carry a revision and are refused when
  stale, so two dashboard windows don't overwrite each other.
- **Voiceprints** are separate files, mode 0600, written durably, as custom
  wake words are. Each records:
  - **The sha256 of the model that made it.** A voiceprint from another model
    is ignored, that person counts as not enrolled, and the dashboard asks them
    to enroll again.
  - **The capture version:** a number pinnad bumps when an update changes the
    microphone processing, such as the front end or the gain. A voiceprint from
    an older capture version is still used, and the dashboard suggests
    enrolling again.

  No audio is kept to redo this automatically.
- **Removing a person deletes their voiceprint.** A factory reset deletes all
  of them. Flash storage can't promise the bytes are overwritten, and the
  dashboard doesn't claim it.
- **Voiceprints never leave the satellite:** not to Home Assistant, not to the
  model service, not to the browser. A future settings backup (ADR 0019) leaves
  them out; re-enrolling takes a minute. The model service gets only the name
  and the extra instructions, as part of the prompt. The People page says so.
- **Removing a person while a request runs** lets that request finish with the
  profile it started with.

### The debugging timeline (amends ADR 0036)

Under "Phases and events", which need nothing beyond the dashboard:

- **Each recognition run**, as a span:
  - when it started and ended;
  - the seconds of speech it used;
  - the outcome: the person's name, uncertain, too short, no model, or
    failed;
  - the best score and the margin to the second-best;
  - run 1 or 2.

  pinnad's log line for the request carries the same outcome and name.
- **Which profile the request ran with**, and why: recognized, carried over
  on a follow-up, person removed, or why nobody was recognized: too late
  (recognition hadn't finished when speech-to-text returned), uncertain, too
  short, no model, recognition failed.
- **Enrollment**, as a span, with its outcome or the failed check.
- **The model loading**, as a span, and a failure to load.

No voiceprint, audio or score history goes into the timeline beyond these
numbers.

### The model and the runtime

- **CAM++ advanced**, shipped in the image under `/usr/share/pinna/models/`
  with its sha256, through `scripts/fetch-models.sh`.
  - Its weights are stored as float16, with casts back to float32
    (`scripts/speaker-f16.py`, run by `scripts/fetch-models.sh`): 14.6 MB
    instead of 28 MB. The voiceprints match the float32 model at cosine
    0.999995 on the wake word test recordings, at the same speed
    (2026-11-28). Computation stays 32-bit.
  - Every owner carries it, including those who never enroll anyone. That
    keeps enrollment working offline.
  - THIRD-PARTY-NOTICES credits it (Apache 2.0).
- **tract 0.23**, without `multithread-mm` (that feature adds 4.7 MB) and
  without the default `transformers` feature. One thread is enough, and pinnad
  keeps the other cores for audio.
  - The wake word's vendored tract-tflite shrinks to one patch
    (SQUARED_DIFFERENCE). 0.23 already has the other two.
  - The upgrade lands first, on its own, and `just wakeword-parity` must pass:
    it changes the wake word for every owner.
- **The front end** is the spike's Kaldi filter bank. It uses `rustfft`,
  already in `Cargo.lock` through tract.
- **The code** is in a new library crate, `pinna-speaker`: the front end, the
  model, matching and enrollment checks, with typed errors. pinnad owns the
  thread, the people list and the RPCs.

### On the dashboard

- **A People page under Voice & sound, for both modes:**
  - the list, with Everyone else;
  - adding, renaming and removing a person;
  - enrolling;
  - each profile's extra instructions;
  - from release 2, each profile's tools;
  - the close-pair warning, and from release 2 the narrower-than-Everyone-else
    warning.
- The page says plainly:
  - "Voice recognition is a convenience, not a lock: a recording or a similar
    voice can pass as someone else."
  - Set limits on Everyone else.
  - Voiceprints stay on this satellite, and each person enrolls on each
    satellite.
  - Home Assistant keeps a history of who spoke.
- **The conversation setting** is a switch on the standalone Conversation
  page, "Continue the conversation when someone else speaks", off by default
  (a new speaker starts a new conversation).
- **Metrics** (`pinna_recognition_*`), without names, so the threshold can be
  tuned: each run's outcome (person, uncertain, too short, no model, failed),
  the score and the margin, how long a run took, and for each standalone
  request why it got its profile, too late included.

## Consequences

- **Memory:** on the Echo Dot (2nd gen), pinnad is about 45–50 MB without the
  model and about 72–85 MB with it loaded. Loading peaks at about 116 MB,
  under its 200 MB limit (2026-11-28). The first build parsed the model twice
  and peaked at 162 MB, 188 MB with an enrollment at the same time. With
  nobody enrolled, nothing changes.
- **CPU:** about 0.76 s of one core per run on the Echo Dot (2nd gen); two runs
  at most per request, overlapping speech and speech-to-text.
- **Image:** 16 MB for every owner: the model and tract 0.23. The armhf root
  grew from 99.4 MB to 115.4 MB (115,363,840 bytes, 2026-11-28), so the Echo
  Dot (5th gen)'s 131.1 MB slot keeps 15.7 MB free.
- **New in release 1:**
  - `pinna-speaker`, people RPCs, the People page and the enrollment flow;
  - the `ENROLL` confirmation purpose;
  - the `last_speaker` entity;
  - the prompt change, the conversation rule and its setting;
  - the timeline spans and the metrics.
- **Tests:**
  - unit tests for the front end against reference features, and for matching
    and the conversation rule;
  - pinnad's spawner and Brain tests for a recognized speaker, a short
    follow-up and a speaker change within the window, and `just e2e` with
    nobody enrolled (Everyone else's instructions reach the brain);
  - `just esphome-conformance` for the entity;
  - `just web-check` for the dashboard;
  - parity of the front end and model on armv7;
  - `just wakeword-parity` for the tract upgrade.
- **Website:** a People page for owners, the Home Assistant page for the
  entity, and the standalone page for the prompt and the conversation.
- **A model change** asks everyone to enroll again.

## Open before building

- **The recordings decide the model.** The owner records on the Echo Dot (2nd
  gen):
  - Enrollment takes for each enrolled person on one day. Commands on another
    day: near (1 m), far (3–4 m), with music, with a TV or radio talking.
    Commands are of natural length, not padded to a fixed time.
  - Voices that aren't enrolled: a guest or another family member if
    possible, and TV or radio speech. With one person enrolled, the threshold
    alone decides, and these trials test it.

  The test passes when:
  - near: at least 95% recognized, and at most 1 taken for someone else in
    100 or more trials, the voices that aren't enrolled included (that bounds
    the rate under about 5%; the metrics after release narrow it);
  - far or with music: at least 85% recognized;
  - timing: with recognition starting at the first pause, at least 95% of
    commands have a result within the 800 ms silence wait plus 300 ms, a fast
    speech-to-text service, on the Echo Dot (2nd gen).

  If CAM++ misses this, try more audio first: the wake word plus up to 3 s,
  which takes about 1.5 s. Failing that, narrow the feature, for example to
  "Last speaker" only.
- **Other models:** the Echo (2nd gen) and the Echo Dot (5th gen) need their
  own takes before the feature is called supported there.
- **Timing on the Echo Dot (5th gen)** and, once it boots, the Echo Dot (3rd
  gen).

## Alternatives not taken

- **ReDimNet2.** It is 1.9–5.6 times slower on the satellites, because tract
  has no 32-bit ARM kernel for depthwise convolutions. Writing one, in
  assembly since 32-bit ARM NEON intrinsics are unstable in Rust, would only
  bring B1 level with CAM++.
- **Hosted recognition.** It sends voices off the device, and the options are
  retired, job-based or proprietary.
- **Inferring gender or age.** It's often wrong, it's sensitive, and nothing
  needs it: the extra instructions say how to address someone.
- **Recognizing from the wake word alone** (about 0.6 s). It's less accurate.
  It can join the command audio later if the recordings show it helps.
- **More than one input length** (1.5 s and 3 s). Each prepared plan costs
  memory and about 9 s to prepare.
- **Starting recognition only after the request ends.** On short commands it
  would race speech-to-text and often lose.
- **Treating an uncertain result as a new speaker.** Short follow-ups would
  reset the conversation mid-task.
- **Typed questions as Everyone else.** It would limit the one person who can
  change every setting anyway.
- **One prompt field that both the owner and the person edit.** A person could
  talk the owner's rules for them out of it.
- **A second, stricter threshold before a person's wider settings apply.** The
  threshold, the margin and Everyone else already handle doubtful matches.
  Revisit if the recordings show confusions near the threshold.
- **An ESPHome event entity instead of the text sensor.** Its event types are
  fixed when the entity is announced, so adding a person would change the
  entity. Clearing the text sensor at each request already makes repeats
  visible.
- **"Forget my voice" by voice.** A mix-up would delete someone else's
  voiceprint. Removal is on the dashboard.
- **Keeping enrollment audio** to re-enroll after a model change. That would
  be biometric audio at rest, for a minute's convenience.
- **Downloading the model when the first person enrolls.** Owners who never
  enroll would save 14 MB, but enrollment would need the internet and a place
  to host the model.
- **ONNX Runtime instead of tract.** It's a C++ dependency and bigger, and
  tract already runs the wake word.
