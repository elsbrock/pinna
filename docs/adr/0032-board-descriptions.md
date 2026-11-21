# ADR 0032: One description per board, and drivers that read it

Status: proposed (2026-11-21), accepted (2026-11-21), built (2026-11-21),
with the tests that run without hardware and the virtual Echo Dot. Not run
on an Echo. Prepares the port in ADR 0031.

## Context

- What Pinna knows about its hardware is spread over the code. On `main`
  (33070d9):
  - The model ids `echo-dot-2` and `echo-2` are matched as strings in
    pinnad's identity, its Sendspin player, the updater and the release
    tool. The owner-facing names are written out in three places,
    each slightly differently.
  - The microphone switch's sysfs files, the mute lamp and the LED ring's
    I2C scan are constants in `pinna-hal`. So are the key codes, with the
    MT8163 rule that KEY_POWER is the microphone button.
  - The sound card's id (`mt-snd-card`) is in pinnad's backend choice. The
    PCMs (`hw:0,23`, `hw:0,24`) are defaults in `pinna-audio`. The
    microphone geometry is in the beamformer. Which speaker tuning a model
    gets is an `if` in pinnad's `main`.
  - The `new-device` skill lists ten places to change for a new model of
    the same SoC.
- ADR 0031 proposes a second SoC, whose boards differ from the MT8163 and
  from each other: another LED controller (one RGB LED on the Echo Input),
  another microphone switch driver (`gpio-privacy`), no volume keys on the
  Echo Input, other PCMs and sample formats, 4 or 7 microphones in unknown
  places, another speaker amplifier and other mixer controls.
- pinna-init and pinna-bootctl run in the initramfs, and pinna-image also
  runs on the developer's computer. They can't take `pinna-hal`'s tokio and
  evdev along.

## Decision

- **A crate, `pinna-board`, describes the hardware**, with the standard
  library only.
  - `Model` names each model Pinna runs on: its stable id, the name owners
    know it by, the name Home Assistant shows, and the short name in a
    default friendly name.
  - `Board` is one static description per model:
    - identity: the idme `device_type_id` and the fastboot product;
    - the SoC and its device tree `compatible`;
    - the ring: its driver, its LEDs, its front centre;
    - the key codes and the button each one is;
    - the microphone switch: the state and trigger files, the mute lamp;
    - the sound card: its id, the playback and capture PCMs with rate,
      channels and format, which mixer sequences set it up, and the speaker
      tuning;
    - the microphones: how many, the reference lanes, the geometry, the
      lanes used for speech;
    - the Wi-Fi chip.
  - `detect()` returns the board from the idme record, else from the device
    tree. An idme id Pinna doesn't know gives no board.
- **`pinna-hal` is the drivers, and it re-exports `pinna-board` as
  `pinna_hal::board`.** A driver takes what it needs from the board:
  `led::open(board)` picks the ring driver, `button_stream(keys)` watches the
  board's keys, and the privacy functions take the board's files.
  - Where boards need different code, the description names the driver as
    an enum variant: the ring, the codec's mixer sequences, the Wi-Fi chip.
    A board with a new LED controller adds a variant and its driver, and
    the compiler finds every place that chooses.
  - Where boards differ only in files, the description names the files.
    The microphone switch is such a case: Amazon's `gpio-privacy` on the
    MT8516 has a state file and an `enable` file that only closes the
    latch, as `amz_privacy` on the MT8163 has. The evidence is
    `drivers/input/keyboard/gpio_privacy.c:378-428` in Amazon's 4.4 tree
    (ADR 0031).
- **`detect()` reads the board once per process, and pinnad wires the
  drivers from it:** the names Home Assistant and Sendspin see, the speaker
  tuning, the PCM device names, the keys, the microphone switch and the
  mute lamp. The updater, the release tool and pinnactl take model ids and
  names from `Model`.
- **Hardware Pinna doesn't know behaves as before.** A computer, the
  virtual Echo Dot and an Echo with an unknown idme id have no board.
  - They report no model id to the control API, Sendspin and the updater,
    so a release that names its models isn't installed on them. The
    virtual Echo Dot reports `echo-dot-2` to the control API, for the
    dashboard's drawing, as before.
  - Home Assistant's model name and the default friendly name say Echo Dot,
    as before.
  - Their drivers use the Echo Dot (2nd gen)'s paths, keys, PCMs and
    tuning. A computer or the VM finds no hardware there; an MT8163 with an
    unknown idme id finds its ring, switch and sound card, as before. The
    virtual Echo Dot keeps its own buttons, latch and audio (ADR 0014).
- **A fact the code still holds as a constant has a test that every board
  matches it:**
  - the 12 logical pixels the IS31FL3236 driver writes, and their front
    centre;
  - the 7 microphones the calibration is read for, and the beamformer's
    geometry for them;
  - the capture format the front end decodes;
  - the playback format, and the MT8163 PCMs' status files that the
    speaker's clocks read.

  A board that differs fails these tests. The code is made general then,
  against that board's measured facts, not before.
- **The ids stay as they are**: `echo-dot-2` and `echo-2` in the proto,
  release manifests, the dashboard's drawings and the metrics.
- **Not in this step:** the image and boot side still name the MT8163 in
  their own files: the USB gadget's product string, the `ConditionFirmware`
  of the Wi-Fi and PipeWire units, `wifi-up`, the PipeWire graph,
  pinna-bootctl's boot-stage ring and Wi-Fi check, `pinna-bootimg`,
  `scripts/flash-gen2.sh`, the browser installer and the dashboard's
  drawings. They move to the board description when the MT8516 port needs
  them (ADR 0031).

## How the design keeps the goals

- **Make development pleasant:** the Rust code learns a new board from one
  description and its drivers. The `new-device` skill's list of places that
  name a model shrinks from ten to seven; the rest are the image, boot and
  web files this step leaves.
- **Be safe to install and update:** the existing satellites behave the
  same. The values moved unchanged, and each one has a test.

## Consequences

- Adding a model of a known SoC is a `Model` variant and a `Board` in the
  Rust code, plus the image, boot and web files the `new-device` skill
  lists.
- `pinna-audio` doesn't depend on `pinna-board`. pinnad hands it the PCM
  device names and the tuning. Its tests, through a development
  dependency, check its constants against every board.
  The mixer sequences stay in `pinna-audio`. The board names them
  (`Codec`); with one codec nothing chooses yet, and a second makes pinnad
  choose by it.
- A second SoC adds variants to the driver enums (ring, codec, Wi-Fi),
  names its switch's files, and makes the ring size, the microphones, the
  PCM formats and their status files data where the tests above fail.
