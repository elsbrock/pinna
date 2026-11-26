# Pinna: product

**Who it is for.**
- Home Assistant users who own an Echo Dot (2nd gen), an Echo (2nd gen) or an
  Echo Dot (5th gen) and want to use it as a voice satellite without Amazon.
- Owners of the same devices who don't run Home Assistant and want a voice
  assistant without Amazon, using services they choose: timers, the weather,
  radio and questions to a model (standalone mode, ADR 0017). This is the
  first audience standalone mode serves.
- People who run an AI agent such as Hermes or OpenClaw and want the device as
  its voice and speaker. Standalone mode serves them too, second.

All of them are comfortable with a terminal, unlocking the device (Kaeru on
the 2nd gen, firebreak on the Echo Dot (5th gen)), fastboot or adb, and
building an image, and they accept early-stage software.

**What it does for them.**
- In Home Assistant mode it turns the device into a Home Assistant voice
  satellite: the wake word runs on the device, requests go to their own Home
  Assistant, and the ring, buttons, mute, volume, timers and media are Home
  Assistant entities.
- In standalone mode the device runs the request itself. It sends
  requests to the speech-to-text, language model or agent, and text-to-speech
  services the owner sets up on the dashboard. It keeps timers on the device,
  tells the weather, plays radio streams, and offers its tools to agents over
  MCP.

**Goals, later and out of scope.** These live in
[`website/src/content/docs/goals.md`](website/src/content/docs/goals.md),
published as the Goals page on the website. In short:
- Goals:
  - your voice goes only where you send it;
  - be a good Home Assistant satellite;
  - play music without getting in the way;
  - work without Home Assistant;
  - be safe to install and update;
  - use standard parts;
  - make development pleasant;
  - stay open.
- Later: more Echo devices, and perhaps a Raspberry Pi if people ask for it.
- Out of scope:
  - replacing Alexa;
  - a Pinna account, cloud service or remote access;
  - a general-purpose distribution.
