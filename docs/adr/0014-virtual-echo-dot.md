# ADR 0014: A virtual Echo Dot in a VM, driven from the dashboard

Status: accepted (2026-11-15). Amends, for the x86_64 image only, ADR 0005
(what the web listener serves) and ADR 0004 (PipeWire runs there too, with a
virtual graph).

## Context

Trying a change to the voice path needs an Echo Dot on the desk: flash or
`just push`, speak to it, watch the ring. The x86_64 image (`just image-x86`)
runs the same Debian userland and pinnad under KVM, but only as a boot gate:
it has no microphone, no speaker, no buttons and no privacy switch, so the
wake word and everything after it are off.

pinnad also runs on a PC with the `pipewire` backend and `--stdin-buttons`.
That skips everything the image adds (systemd units, PipeWire as a system
service, `/var` state, updates) and has no buttons for long presses or the
privacy latch.

## Decision

`just dev-vm` boots the x86_64 image as a **virtual Echo Dot**. The browser
that opens its dashboard acts as the Dot's microphone, speaker and buttons.

- **The VM.** KVM, a persistent disk under `build/dev-vm/` (the Home Assistant
  key and settings on `userdata` survive restarts), and a newly built image
  rewritten into `system_a` on every start. Every start also resets the slot
  state in `misc`, so the newest image boots even after an update in the VM
  switched to slot b. The dashboard and the ESPHome API are forwarded to
  `127.0.0.1` only. `ESPHOME_BIND=0.0.0.0` opens the ESPHome API to the LAN
  for a Home Assistant there, added by `host:port`. mDNS does not leave QEMU's
  user network.
- **A Home Assistant on the same computer.** `just dev-ha` runs Home
  Assistant, Wyoming Whisper (speech to text) and Piper (text to speech) in
  containers on the host network, versions pinned in the script. It onboards
  Home Assistant with a generated local account, kept in `build/dev-ha/`.
  Then it adds Whisper, Piper and the virtual Dot (`127.0.0.1:6053`) and
  makes an Assist pipeline of them the preferred one. Each step is skipped
  when it is done already. Home Assistant listens on loopback only. Logins
  from loopback bypass the password (`trusted_networks`). The satellite
  fetches replies from `http://10.0.2.2:8123`, its `internal_url`, which
  QEMU maps to the host's loopback. A developer's own Home Assistant is often
  on another VLAN, behind the computer's firewall. This one needs neither.
- **Its own identity.** The x86_64 image does not carry `secrets/device/`
  (node name, ESPHome key), so it never joins Home Assistant as the owner's
  Dot. Its MAC comes from the image's machine id, as for any non-Dot pinnad,
  so it is the same device in Home Assistant across rebuilds.
- **Virtual hardware, behind `[dev] virtual_device`.** Only the x86_64 image
  sets it (`/etc/pinna/vm.toml`, layered over `pinnad.toml` by a
  `pinnad.service` drop-in). It goes through the paths pinnad uses on the
  Dot, so pinnad's voice path does not change. Only its startup picks the
  virtual parts instead of the Dot's, and it reports its model as
  `echo-dot-2`, the Dot it stands in for:
  - **Buttons:** a uinput device with the Dot's keys. pinna-hal's evdev scan
    finds it like the Dot's gpio-keys. The x86 kernel gains `INPUT_EVDEV` and
    `INPUT_UINPUT`.
  - **Microphone switch:** a privacy latch file under `/run/pinna/virtual/`,
    polled like the PMIC's `privacy_state`. Its button flips the latch and
    sends KEY_POWER, as the Dot's does. While it is on, browser audio is
    dropped before it reaches PipeWire.
  - **LED ring:** pinnad draws on a ring that publishes its frames instead
    of an IS31FL3236 (`led_backend` "virtual").
  - **Microphone and speaker:** the VM runs PipeWire and WirePlumber
    (`pipewire -c pipewire-vm.conf`, which does not load the Dot's graph).
    pinnad uses its plain `pipewire` backend (ADR 0004: "laptops/VMs"). It
    plays to the default sink, `dev_speaker`, and records `dev_mic` by name
    (`[audio] capture_target`). `dev_mic` is a loopback, which WirePlumber
    treats as a filter and never makes the default source. pinnad plays what
    the browser sends into `dev_mic_in`, looped to `dev_mic`, and sends
    `dev_speaker`'s monitor back. With no browser connected, `dev_mic`
    carries silence.
- **One socket, `/virtual-device`, on the web listener.** It shares the
  dashboard's origin and Host allowlist. Binary frames carry microphone audio
  up (S16LE mono, 16 kHz) and speaker audio down (S16LE mono, 48 kHz). Text
  frames carry button presses and releases up as JSON, and the ring's frames
  down, at most 25 a second. One browser at a time: a
  new connection closes the old one (code 4000). A button held when a
  connection ends is released, unless a newer connection holds it by then.
  Queues are bounded in both directions and drop audio rather than block,
  since the browser's clock and the VM's drift apart. A browser that takes
  more than 2 s to accept a message is disconnected.
- **An Origin check.** WebSockets have no CORS preflight, which is what ADR
  0005 relies on against cross-site requests. The socket accepts a browser
  only from the dashboard's own origin or from a page on the same computer
  (the Vite dev server). Clients that send no Origin (tests) are accepted.
- **Never on a real Dot.** pinnad refuses `virtual_device` when it finds real
  hardware (the MT8163 device tree). The Dot's kernel has no uinput either.
- **The dashboard.** `Status.virtual_device` tells it to show a panel for the
  virtual Dot. Connect also asks for this browser's microphone. The panel can
  play an audio file as the microphone, for repeatable wake word runs (it
  replaces the live microphone while it plays). It plays the speaker, 12 dB
  up, so that volume 100 is full scale. The four buttons go down and up with
  the pointer. While connected, the Overview's drawing of the Dot shows the
  ring's frames instead of the dashboard's own drawing from `Status`.

## Consequences

- The voice path can be tried without the Dot: wake word, Home Assistant's
  pipeline, the speaker, timers, media, the buttons and the privacy switch.
  Not the beamformer or the Dot's echo canceller. A browser has one
  microphone channel, and its own echo cancellation keeps the speaker out of
  the microphone.
- The browser only grants its microphone to a secure context. That means
  `http://localhost` or `127.0.0.1`. A browser on another computer needs an
  SSH tunnel to the dashboard port.
- With `ESPHOME_BIND=0.0.0.0`, the VM's ESPHome API is plaintext on
  whatever network the computer is on, like a freshly flashed Dot. That lasts
  until Home Assistant sets a key.
- `just dev-ha` needs Podman or Docker, and its first start downloads a few
  gigabytes of images and models. Its Home Assistant is a throwaway: nothing
  is exposed beyond loopback, and `FRESH=1` starts over.
- Anyone who can reach the VM's dashboard can play audio into its microphone
  and hear its speaker. The forward is on loopback, and the Origin check
  keeps web pages out, but local processes are trusted.
- The x86_64 boot gate now also runs PipeWire and the virtual hardware, so it
  checks them too.
