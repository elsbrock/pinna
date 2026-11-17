---
name: run-pinna
description: Start Pinna to see a change working and check it in a browser. Covers the dashboard with live reload, the virtual Echo Dot (an x86_64 VM) with a local Home Assistant, the website, and new binaries on a real device. Use when asked to run, start, try, demo or screenshot Pinna, the dashboard or the website, or to show that a change works beyond the tests.
---

# Run Pinna

`website/src/content/docs/development.md` is the reference for everything
here. This skill picks the right surface and lists the traps.

If `just`, `cargo` or `npm` isn't on `PATH`, run the commands through
`nix develop -c` (see `AGENTS.md`).

## Pick the smallest surface that shows the change

| The change | Run |
| --- | --- |
| Dashboard layout or text, no new RPC | Vite against the virtual Echo Dot |
| `pinnad`: RPCs, the ring, voice, settings | The virtual Echo Dot, with `just push-vm` for new binaries |
| Audio, Wi-Fi, LEDs, buttons, boot on real hardware | A device with `just push`, only when the owner asks |
| The website | `cd website && npm run dev` |

## The virtual Echo Dot

Prerequisites (each once):

- `just models`. Without the models the wake word does nothing, and that's
  not a bug in your change.
- `just kernel-x86 image-x86`. Run `just image-x86` again after changes to
  the image.
- A signing key (`just keygen`) and `secrets/authorized_keys`.
- Write access to `/dev/kvm`. Without KVM, the satellite takes many minutes
  to start.

Then:

1. Start `just dev-vm` in the background. It keeps running. Wait until
   `http://127.0.0.1:18090` answers.
2. If the change involves Home Assistant, start `just dev-ha` in the
   background as well (Home Assistant, Whisper and Piper in containers;
   `http://localhost:8123`, no login from this computer). The first start
   downloads several gigabytes. There is one per computer, and starting it
   from another worktree replaces it.
3. After a Rust change, run `just push-vm`. The new binaries last until the
   VM restarts.

- SSH: `ssh -p 2223 root@127.0.0.1`.
- From a second worktree, give the VM its own ports:
  `WEB_PORT=18190 SSH_PORT=2323 ESPHOME_PORT=6153 just dev-vm`, and
  `SSH_PORT=2323 just push-vm`.
- `FRESH=1 just dev-vm` starts with an empty `/var`.
- A Home Assistant on another computer can't discover the VM. Start it with
  `ESPHOME_BIND=0.0.0.0 just dev-vm` and add it by address and port 6053. The
  computer's firewall must allow that port: ask the owner to open it rather
  than changing firewall rules yourself.

## The dashboard with live reload

```sh
cd dashboard && PINNA_URL=http://127.0.0.1:18090 npm run dev -- --port 5173 --strictPort
```

Vite proxies the API to `PINNA_URL`: the virtual Echo Dot, or a satellite at
`http://<node name>.local`. Without `PINNA_URL` it expects a `pinnad` on
`127.0.0.1:7470`. Pages are routed by hash: `#/voice`, `#/voice/wake-word`,
`#/voice/media-sources`, `#/home-assistant`, `#/network`, `#/system`,
`#/system/logs`.

Browsers give the microphone only to `localhost` and `127.0.0.1`, so the
virtual Echo Dot's microphone works only on those addresses.

## Check it in a browser

When `playwright-cli` is available, use it to look at the result before
saying that the change works:

1. Open the page, then take screenshots at a phone width (390 px) and a
   desktop width (1440 px), in light and dark themes when colours changed.
2. Read the screenshots. Check the browser console for errors.
3. For anything live (the ring, phases, toasts), act on the virtual Echo Dot
   from the Overview (buttons, **Audio file as the microphone**) and capture
   the state you meant to change.

Save screenshots and logs in a temporary directory, not in the repository.

## A real device

Only when the owner asks, and say which device you used.

- `just push` cross-compiles `pinnad`, `pinnactl`, `pinna-bootctl` and
  `pinna-bootstages` and installs them as a system extension, then restarts `pinnad`. Without
  `PINNA_HOST` it connects over USB to `root@10.42.0.1`; otherwise set
  `PINNA_HOST=root@<address>`.
- `scripts/push-dev.sh --remove` goes back to the installed programs.
- `just test-device` is the hardware check: speaker, microphones, ring and
  the Home Assistant API.

## Afterwards

Stop what you started: the VM, Vite, and `just dev-ha stop` if you started
Home Assistant. Leave alone anything that was already running.
