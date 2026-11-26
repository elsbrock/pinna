---
name: new-device
description: Bring Pinna up on another Echo model, or find out why a device doesn't boot. Covers what to check before writing anything, every place in the code and docs that knows the model, and how to debug a boot without a serial console (the ring's boot stages, rescue mode, pstore). Use when adding a device, porting the kernel, or when a device hangs, loops or ends in fastboot or rescue mode.
---

# Support a new device

Pinna runs on two MT8163 models: the Echo Dot (2nd gen, fastboot product
`BISCUIT`) and the Echo (2nd gen, `RADAR`). Both boot the same kernel, device
tree (`mt8163-radar-puffin.dtb`), boot image and root filesystem. The
bootloader's idme record tells them apart. This skill follows the path the
Echo (2nd gen) took. The Echo Dot (5th gen) is an MT8519 on Amazon's U-Boot
(ADR 0035); where it differs from the MT8163 Echos, the steps below say so.

## 0. Decide whether this is a port or a model

- **Another MT8163 board**: mostly identification, audio tuning and docs.
  Follow this skill.
- **Another SoC** (for example a later Echo generation): a new kernel port,
  device tree, boot chain and possibly partition layout. Write an ADR first
  (see `docs/adr/0001-platform.md` and `0006-kernel.md`) and agree on it with
  the maintainer before you write code.

## 1. Before anything is written to the device

- The board's `bootloader` in `pinna-board` decides what follows. On the
  Kaeru payload (`Bootloader::Kaeru`, the MT8163 and MT8516 Echos) Pinna
  relies on it for unsigned boot images, fastboot and the fastboot guard. On
  Amazon's U-Boot (`Bootloader::AmazonUboot`, the Echo Dot (5th gen)) there
  is no fastboot: the boot image is a signed FIT, the way back is the
  community installer's preloader route over the USB test pads, and the
  storage is UBI volumes, not a GPT. Take the bootloader, the storage kind
  (`StorageKind`) and the slot sizes from the device and record them in the
  `Board`.
- On a Kaeru board, find out how to reach fastboot on this model (the Echo (2nd gen) has no USB port and needs pins
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

The Rust code learns a model from one description (ADR 0032). The rest of
the repository still names models in its own files. Go through all of them:

| Where | What |
| --- | --- |
| `crates/pinna-board/src/lib.rs` | A `Model` variant with its id and names, and its `Board`: the idme `device_type_id`, the fastboot product, the ring, the keys, the microphone switch, the sound card, the speaker tuning and the microphones. Read the key codes from the device's input events; don't assume them. Measure loudness against the firmware the device ran before you pick the tuning. The tests say what else must change |
| `crates/pinna-audio/src/beamformer.rs`, `dsp.rs` | Their tests fail when a board's microphones or capture format differ from the ones the front end is built for. Make the code take the board's facts then |
| `proto/pinna/v1/device.proto` | Document the model id, then regenerate `dashboard/src/gen/` |
| `dashboard/src/lib/view.ts` | `satelliteModel` maps the board name Home Assistant shows to the drawing's model id; an unknown name draws the Echo Dot (2nd gen) |
| `crates/pinna-ladspa/` | `pinna_wake` and the beamformer read the board description when PipeWire loads the plugin (`wake.rs`, `plugin.rs`). Check the plugin's inputs against the board's lanes |
| `mkosi/mkosi.extra/usr/libexec/pinna/pipewire`, `mkosi/mkosi.extra/etc/pipewire/` | One read-only root serves every SoC. The wrapper picks the PipeWire graph from the device tree's SoC and sets the capture routes before PipeWire opens the microphones. A new SoC needs its own `pinna-<model>.conf.d` graph that keeps the node names `dot_mics` and `dot_wake`, a branch in the wrapper, and a link in `mkosi.postinst.chroot` |
| `mkosi/mkosi.extra/usr/libexec/pinna/wifi-ap` | The setup hotspot's `ap0`: through `/dev/wmtWifi` on the MT8163 (`Wifi::Mt8163Consys`), `iw dev wlan0 interface add` on mac80211 chips (`Wifi::Mt7663Sdio`), which `pinnactl wifi-chip` names. A new Wi-Fi chip needs a branch, and `Wifi::supports_sae` an answer |
| `crates/pinna-bootctl/src/board.rs` | The boot stages' skips: the Wi-Fi stage applies on every known board, and the USB stage is skipped without a UDC. Stage 4 is skipped only when the kernel registers no UDC |
| `crates/pinna-image/`, ADR 0035 Decision 9 | A release names the models it is for (`requires.models`) and carries a boot image per boot format (`boots`); the Android image and the FIT are separate. Sealing the root and building the manifest fail when an image is larger than the smallest slot of its models. Add the model to the release and give it a boot image |
| `crates/pinna-init/src/rescue.rs`, `crates/pinnad/src/wifi.rs` | Owner text that names fastboot, Volume Down or USB follows `board.bootloader`. Check it says nothing that is false on the new board |
| `mkosi/mkosi.extra/usr/libexec/pinna/usb-gadget` | The USB product string |
| `scripts/flash-gen2.sh` | Accept the fastboot product in every subcommand |
| `design/devices/<model>.ts`, `design/devices/index.ts` | A drawing of the device for the dashboard and website |
| `kernel/patches/` | The vendor Wi-Fi driver takes Amazon's Echo settings only for the project name "radar" (patch 0028). Check that a new board takes the same branches |

An unknown idme id gives no board (`pinna_board::detect`), and an MT8163
without the record is the Echo Dot. Keep it that way, so existing devices
never change. An unknown board on another known SoC starts pinnad without
audio and ring drivers.

## 3. Bring it up

The MT8163 Echos have no serial console; find out whether the new board has
one before you rely on that. Where there is none, use these channels instead of guessing from LED colours:

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
