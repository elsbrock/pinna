# Pinna: product

**Who it is for.** Home Assistant users who own an Echo Dot (2nd gen) or an
Echo (2nd gen) and want
to use it as a voice satellite without Amazon. They are comfortable with a
terminal, fastboot and building an image, and they accept early-stage software.

**What it does for them.** It turns the device into a Home Assistant voice
satellite: the wake word runs on the device, requests go to their own Home
Assistant, and the ring, buttons, mute, volume, timers and media are Home
Assistant entities.

**Goals, later and out of scope.** These live in
[`website/src/content/docs/goals.md`](website/src/content/docs/goals.md),
published as the Goals page on the website. In short: keep speech local, be a
good Home Assistant satellite, be safe to install and update, use standard
parts, make development pleasant, stay open. Later: more Echo devices and a
standalone mode. Out of scope: replacing Alexa, cloud accounts and remote
access, a general-purpose distribution.
