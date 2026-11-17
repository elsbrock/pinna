# ADR 0013: Several wake words, two at a time, custom ones from the dashboard

Status: proposed (2026-11-15), accepted in part (2026-11-17). Amends ADR 0007; builds on ADR 0005.

Built: several words with at most two active, the built-in table, `wake_words`
in `settings.toml`, the dashboard choosing only while Home Assistant isn't
connected, the detection rules and the patched `vendor/tract-tflite`. Not
built yet: custom words from the dashboard (`AddWakeWord`, `RemoveWakeWord`,
`/var/lib/pinna/wakewords/`, the loading mark) and the per-word peak score;
the near-miss log line names the word that scored instead.

## Context

- pinnad listens for one word. `[wake] keyword` names one openWakeWord classifier (`alexa_v0.1.onnx`),
  and pinnad offers only that word to Home Assistant.
- openWakeWord runs in two parts. The mel spectrogram and the embedding are the same for every word;
  each word is a small classifier on the last 16 embeddings (`[1,16,96]` → one score). A second word
  costs one more small classifier per 80 ms block.
- Home Assistant shows two wake word dropdowns for every ESPHome satellite ("Wake word" and "Wake word 2").
  It reads the device's list of words and its active words when it connects, and after it sets them itself.
  It never sends its own choice when it connects.
- Those dropdowns can't show a word chosen on the device. When Home Assistant connects, the first dropdown
  adopts the device's word only if exactly one is active and the dropdown is empty; otherwise a dropdown
  whose word isn't active shows "No wake word", and the second never adopts anything. The next change in
  Home Assistant sends only its own dropdowns, so it would drop a word chosen elsewhere.
- Words that come ready-made:
  - Alexa, Hey Jarvis, Hey Mycroft and Hey Rhasspy: the openWakeWord v0.5.1 release, as `.onnx`,
    CC BY-NC-SA 4.0.
  - Okay Nabu, Home Assistant's default: only as `.tflite`, in `rhasspy/pyopen-wakeword`, a repository
    under Apache-2.0 that gives the model no licence of its own.
  - Home Assistant users who trained their own word, or took one from the community collections,
    mostly have `.tflite` files. Some are microWakeWord models (ESPHome devices such as Voice PE), which
    Pinna can't run.
  - openWakeWord's trainer sizes a word's window from its clips: most words read 16 embeddings, a long
    phrase can read more.
- tract reads `.tflite` through `tract-tflite`. Version 0.22.5 gets openWakeWord's LayerNorm classifiers
  wrong:
  - a negative reduce axis panics;
  - a float `MEAN` is rounded to an integer;
  - `SQUARED_DIFFERENCE` isn't supported.
  With these three fixed, tract matches Google's LiteRT to about 1e-6 on Alexa, Okay Nabu and a
  community-trained model.
- ADR 0007 said: "The wake word model: there is one, and Home Assistant chooses wake words."

## Decision

- **Several words, at most two active.** pinnad offers every built-in word whose model file is in the
  image, plus the custom ones. `max_active_wake_words` is 2.
- **The device keeps the active words.** They are a setting in `settings.toml` (`wake_words`, a list of
  ids), so the satellite keeps listening for the same words without Home Assistant.
- **Home Assistant chooses while it is connected; the dashboard otherwise.**
  - Home Assistant's own dropdowns are the Home Assistant entity for this setting (ADR 0007: "unless
    Home Assistant has its own way"), so pinnad adds no entity of its own.
  - While the ESPHome API has a client, the dashboard shows the active words but doesn't change them, and
    `UpdateSettings` refuses a change with a sentence that says to choose in Home Assistant.
  - Without a client (Home Assistant away, or not set up), the dashboard chooses through `UpdateSettings`.
    When Home Assistant comes back, its dropdowns show "No wake word" for a word it didn't choose, until
    someone chooses there; the satellite keeps listening for it meanwhile.
- **`[wake]` in `pinnad.toml` gives the first-boot word.** `keyword`, `id` and `phrase` keep their meaning
  for that word, so existing overrides still parse.
- **Built-in words are a table in pinnad** (id, phrase, file). A word whose file is missing isn't offered.
- **Custom words come from the dashboard.**
  - `AddWakeWord` stores the model in `/var/lib/pinna/wakewords/` with an `index.toml` (id, phrase, file,
    sha256). `RemoveWakeWord` deletes it, and the word leaves the active list.
  - Accepted: `.onnx` and `.tflite`, recognised by their content, at most 2 MiB, at most 8 custom words.
  - The phrase is 2 to 40 characters and unique (ignoring case). Home Assistant maps phrases to ids, so
    two words with the same phrase would collide there.
  - The model has to load as a classifier from `[1,N,96]` floats to one score and run once. A model that
    fails is rejected with a sentence the dashboard shows, naming a microWakeWord model as such, and
    nothing is written.
  - Adding and removing go through one task, so two dashboards can't overshoot the limit or lose a write.
  - Each word reports the peak score of the last utterance it heard, so an owner can tell a word that
    never fires from one that nearly does.
- **A custom word never stops pinnad from starting.**
  - A model that fails to load at start is logged, reported as failed, and dropped from the active words.
    `pinna-boot-ok` doesn't depend on it.
  - pinnad marks a model as loading before it loads it and clears the mark after. A mark left behind
    means the load crashed pinnad (out of memory, an abort), so the next start skips that model.
  - A corrupt `index.toml` means no custom words, logged; a missing model file means that word failed.
- **Detection:**
  - Each active word has its own three-block window, and the detection rule doesn't change.
  - The embedding history is as long as the longest window; each word reads its own last N embeddings.
  - One lockout is shared by all words: one utterance gives at most one detection. When two words fire
    in the same block, the higher score wins.
  - The word that fired is the one reported to Home Assistant (`last_wake_word`, the run's wake word phrase).
  - The sensitivity applies to every word.
- **`tract-tflite` is kept in the repository** as `vendor/tract-tflite/` (0.22.5 plus the three fixes,
  listed in its `PATCHES.md`) through `[patch.crates-io]`. The fixes go upstream; the copy goes when a
  tract release has them.

## Consequences

- Okay Nabu and most community models work without converting anything.
- The checks on an uploaded model run in pinnad, in a blocking task that catches a panic. A model built to
  exhaust memory could still take pinnad down. Running the check in a separate process is left out: under
  ADR 0005, anyone on the LAN can already control the satellite.
- A word chosen on the dashboard while Home Assistant was away shows as "No wake word" in Home Assistant
  until someone chooses there. Fixing that needs Home Assistant's dropdowns to adopt the device's words.
- `wake_words` needs `settings.toml` to ignore keys it doesn't know (ADR 0007), released first, so a
  rollback past this change keeps the other settings.
- One sensitivity for both words: tuning a custom word changes the other one too.
- The built-in openWakeWord models stay CC BY-NC-SA and are fetched at build time, never committed. Okay Nabu
  is fetched the same way, pinned to a commit.
