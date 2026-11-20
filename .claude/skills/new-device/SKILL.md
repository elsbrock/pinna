---
name: new-device
description: Bring Pinna up on another Echo model, or find out why a device doesn't boot. Covers what to check before writing anything, every place in the code and docs that knows the model, and how to debug a boot without a serial console (the ring's boot stages, rescue mode, pstore). Use when adding a device, porting the kernel, or when a device hangs, loops or ends in fastboot or rescue mode.
---

# Support a new device

Pinna runs on two MT8163 models: the Echo Dot (2nd gen, fastboot product
`BISCUIT`) and the Echo (2nd gen, `RADAR`). Both boot the same kernel, device
tree (`mt8163-radar-puffin.dtb`), boot image and root filesystem. The
bootloader's idme record tells them apart. This skill follows the path the
Echo (2nd gen) took.

## 0. Decide whether this is a port or a model

- **Another MT8163 board**: mostly identification, audio tuning and docs.
  Follow this skill.
- **Another SoC** (for example a later Echo generation): a new kernel port,
  device tree, boot chain and possibly partition layout. Write an ADR first
  (see `docs/adr/0001-platform.md` and `0006-kernel.md`) and agree on it with
  the maintainer before you write code.

## 1. Before anything is written to the device

- The device must be unlocked with the Kaeru bootloader payload; Pinna relies
  on it for unsigned boot images and the fastboot guard. Find out how to reach
  fastboot on this model (the Echo (2nd gen) has no USB port and needs pins
  on its base) and record it for `recovery.md`.
- Back up the whole eMMC while the current firmware still runs:
  `scripts/backup-gen2.sh <dir>`, then `scripts/verify-backup.sh <dir>`.
  Both need adb on the current firmware. Without a backup there is no way
  back.
- Collect, from fastboot and the running firmware: the fastboot `product`,
  the idme `device_type_id`, and the partition table. `pinna-init` finds
  partitions by GPT name (`misc`, `boot_a`, `boot_b`, `system_a`, `system_b`,
  `userdata`). If the names differ, stop and ask.
- Check that `system_a`/`system_b` fit the root image and `boot_a`/`boot_b`
  fit the boot image.
- The owner decides when anything is written. Save `boot_a` and `boot_b`
  before you write a boot image.

## 2. Teach the code the model

Each of these knows the model by name. Go through all of them:

| Where | What |
| --- | --- |
| `crates/pinna-hal/src/ident.rs` | `device_type_id` → model id in `board_model_in()`, with a test |
| `proto/pinna/v1/device.proto` | Document the model id, then regenerate `dashboard/src/gen/` |
| `crates/pinnad/src/identity.rs` | `model_name()` (what Home Assistant shows), `default_friendly_name()`, `speaker_flat()`, with tests |
| `crates/pinnad/src/main.rs`, `crates/pinna-audio/src/mixer.rs` | Speaker profile and gain. Measure loudness against the firmware the device ran before you pick values |
| `crates/pinna-audio/src/beamformer.rs` | Microphone positions, if the array differs |
| `crates/pinna-hal/src/buttons.rs` | Key codes, read from the device's input events, not assumed |
| `mkosi/mkosi.extra/usr/libexec/pinna/usb-gadget` | The USB product string |
| `scripts/flash-gen2.sh` | Accept the fastboot product in every subcommand |
| `design/devices/<model>.ts`, `design/devices/index.ts` | A drawing of the device for the dashboard and website |
| `kernel/patches/` | The vendor Wi-Fi driver takes Amazon's Echo settings only for the project name "radar" (patch 0028). Check that a new board takes the same branches |

An unknown idme id reports an empty model, and a board without the record
reports as the Echo Dot. Keep it that way, so existing devices never change.

## 3. Bring it up

There is no serial console. Use these channels instead of guessing from LED
colours:

- **The ring's boot stages** (ADR 0012, `ring-and-buttons.md`). The ring
  fills as stages pass, and white flashes give the number of the stage that
  missed its deadline: 1 storage, 2 root, 3 system, 4 USB, 5 Wi-Fi driver,
  6 `pinnad`, 7 healthy. The stage where it stops is the diagnosis.
- **Rescue mode** (`recovery.md`). When the storage, the root or the system
  fails, the device stays up with a green ring. It serves its log and a
  diagnostics download at `http://10.42.0.1`, and a root shell on the USB
  serial console.
- **pstore** survives a warm reset only. Let the failing boot restart while
  you hold Volume Down, without unplugging. The device opens fastboot with
  RAM kept. Write a boot image that works (the bootloader can't
  `fastboot boot`), start it, and read `/var/lib/systemd/pstore/`.
- **The previous boot's journal**: `journalctl -b -1` from a working boot,
  once the failing boot got as far as systemd.
- **No replugging**: from a running system, `pinna-bootctl reboot-bootloader`
  opens fastboot.
- `/proc/idme` exists only with Amazon's idme driver, which Pinna's 6.12
  kernel doesn't carry. LK must publish the
  idme record in the device tree, or the model reads as the Echo Dot.

## 4. Verify on the hardware

1. Every boot stage passes and the slot is marked good (`pinnactl status`).
2. `just test-device`: speaker, microphones, ring and the Home Assistant API.
3. An update installs into the other slot, and a broken one rolls back.
4. Wi-Fi under load: repeated large downloads after a few seconds idle, with
   no stalls.
5. Wake word, a full voice request, buttons and the microphone switch.

Record which device and which build you tested in the pull request.

## 5. Docs

Adding the Echo (2nd gen) touched 22 public files. Search the website, README
and `PRODUCT.md` for "Echo Dot (2nd gen)" and decide for each place whether
the new model belongs there. At least: `index.md`, the landing page
(`website/src/pages/index.astro`), `status.md`, `goals.md`,
`requirements.md`, `flash.md`, `recovery.md`, `troubleshooting.md`,
`configure.md`, `control-api.md`, `audio.md`, `ring-and-buttons.md`,
`website/src/lib/site.ts`, `website/src/components/Footer.astro`, and
`docs/runbook-first-flash.md`. Follow the `public-docs` skill.
