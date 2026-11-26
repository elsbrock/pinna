# Pinna

An open, local-first voice satellite OS for Echo smart speakers: the
**Echo (2nd gen)** and **Echo Dot (2nd gen)** (MediaTek MT8163), and, with
limits, the **Echo Dot (5th gen)** (MediaTek MT8519). More Echo models are
planned.

Pinna turns the device into a voice satellite with an on-device wake word,
light ring feedback and button control. You choose who answers requests: your
Home Assistant, which adds the satellite over the ESPHome API, or the satellite
itself in standalone mode, with the speech services and the language model or
AI agent you choose, and timers, the weather and radio on the device. It also
plays music over AirPlay and Bluetooth, and from Music Assistant over Sendspin
(experimental), in step with the other speakers of a group. A dashboard on the
device sets it up and manages it from your browser, with or without Home
Assistant. It finds new releases on its update channel and shows them on the
dashboard and in Home Assistant, which install them with one button. It is a
modern image-based Linux system: Debian userland built with mkosi, systemd, a
read-only erofs root in signed A/B slots verified by dm-verity, and small Rust
daemons.

> Status: early development. Owners set up the satellite on its dashboard;
> developers have SSH, `pinnactl` and `pinnad.toml`. Expect sharp edges.

Pinna needs an Echo whose bootloader is already unlocked. On the Echo (2nd
gen) and the Echo Dot (2nd gen), that is amonet with Kaeru, which gives
Pinna fastboot. On the Echo Dot (5th gen), it is the community installer
[firebreak](https://github.com/Gamer92000/firebreak), built with
[cannoli-os-builder](https://github.com/Gamer92000/cannoli-os-builder), and
there is no fastboot. To install Pinna on the first two,
follow `docs/runbook-first-flash.md`: it covers downloading a prebuilt image
from a [release](https://github.com/elsbrock/pinna/releases) or building your
own, backs the device up first, and every step says how to undo it.

The Echo Dot (5th gen) is different. It has no fastboot mode and no USB
connector, and its unlock needs wires soldered to test pads. Pinna doesn't
ship it. Pinna has no install guide
for it yet. What was tried on one unit (2026-11-26): Pinna boots, the ring, the
buttons and the microphone switch work, the speaker plays cleanly, and the
wake word and a voice request through Home Assistant worked. Two of its three
microphones are captured and the echo-cancelling reference isn't, so talking
over music is weak. Updates over the air aren't available on it yet, and the
setup hotspot is unmeasured on its Wi-Fi chip. See ADR 0035 in `docs/adr/`.

## Layout

| Path | What |
|---|---|
| `crates/` | Rust workspace: `pinnad` (device daemon), ESPHome API, wake word, HAL, audio, standalone requests (`pinna-assist`, `pinna-mcp`, `pinna-vad`, `pinna-http`), boot image tooling, `pinna-init` + signed A/B slot images and the updater |
| `keys/` | Release public key (images are signed with `secrets/release.key`, see ADR 0003) |
| `kernel/` | Debian 6.12 source pin, board-support patches for the MT8163 (Echo (2nd gen), Echo Dot (2nd gen)) and the MT8519 (Echo Dot (5th gen)), config fragments and build (packaged as a Debian `.deb`) |
| `mkosi/` | Debian trixie image definition |
| `docs/` | Decision records and runbooks |
| `services/updates/` | The release service behind `updates.getpinna.dev` (Cloudflare Worker): update channels and release downloads, ADR 0022 |
| `releases/` | Release notes, one file per release, drafted by the `release-notes` skill |
| `website/` | Project site (Astro + Tailwind): landing page and docs, published on GitHub Pages |
| `dashboard/` | The dashboard (Svelte, connect-es) that `pinnad` serves on port 80; built into the image by `just web` |
| `design/` | Design system: tokens, component CSS, fonts and the LED ring for the website and dashboard |
| `secrets/`, `blobs/` | Owner-local inputs (credentials, wake word models) — never committed |

## Development

```sh
nix develop      # optional but recommended: dev shell (x86_64 Linux) with toolchains, keyrings, fastboot
just             # list tasks
                 # first: the owner inputs in secrets/ (docs/runbook-first-flash.md, step 2)
just keygen      # optional: your own signing key (secrets/release.{key,pub})
just all         # kernel, wake word models, root filesystem (sealed slot image), boot image
just release     # build/release/: root image, boot image, signed update manifest
just push        # fresh binaries onto a running device (sysext overlay)
just test-device # hardware gate against a running device
just test        # everything: unit/lint, ESPHome conformance, wake word parity,
                 # end-to-end voice flow, armhf + x86_64 VM boot gates, A/B update + rollback
```

See `docs/` for the platform design and the first-flash runbook. The website
lives in `website/` (`npm run dev`); it deploys to GitHub Pages from
`.github/workflows/deploy-site.yml` — see `website/README.md`.

The experimental Echo Dot (5th gen) port uses the UBIFS `userdata` volume
for persistent `/var`. See ADR 0035 in `docs/adr/`.

## License

MIT, see `LICENSE`. Exceptions: `kernel/patches/` and `tools/wmt/` are
GPL-2.0, like the code they patch or come from. Amazon's Wi-Fi/BT firmware and
microphone FPGA bitstream aren't in the repository: `just firmware` fetches
them, unmodified, and the MIT license doesn't cover them.

Parts of Pinna are ported from [LibreEcho](https://github.com/aslater3/LibreEcho):
the kernel's MT8163 board support, the microphone beamformer and DSP, the LED
and button handling, the wake word detection rules and the ESPHome entity set.
Third-party notices for ported and copied material are in
`THIRD-PARTY-NOTICES.md`.
