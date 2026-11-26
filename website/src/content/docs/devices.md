---
title: Echo devices
description: The Echo smart speakers Pinna runs on, what works on each, and where every other Echo without a screen stands.
---

Pinna runs on three Echo devices. This page lists every Echo smart speaker
without a screen, so you can check whether yours runs Pinna before you get
anything else ready.

To find your Echo, look for the model number on the label on its base. The
codename is the device's internal name. On the Echo Dot (2nd gen) and the
Echo (2nd gen), fastboot reports it in capitals, for example `BISCUIT`, which
helps when a label is worn off.

## Supported

| Echo | Year | Model number | Codename | Chip | Bootloader unlock |
| --- | --- | --- | --- | --- | --- |
| Echo Dot (2nd gen) | 2016 | RS03QR | `biscuit` | MediaTek MT8163 | amonet, with Kaeru; [firebreak](https://github.com/Gamer92000/firebreak) also unlocks it |
| Echo (2nd gen) | 2017 | XC56PY | `radar` | MediaTek MT8163 | amonet, with Kaeru; [firebreak](https://github.com/Gamer92000/firebreak) also unlocks it |
| Echo Dot (5th gen) | 2022 | C2N6L4 | `cannoli` | MediaTek MT8519 | [firebreak](https://github.com/Gamer92000/firebreak) |

Only the Echo Dot (5th gen) with model number C2N6L4 is supported, not the
Echo Dot with clock (5th gen) or the Echo Pop.

What each unlock gives Pinna:

- **Echo Dot (2nd gen) and Echo (2nd gen):** amonet unlocks the bootloader and
  installs Kaeru, an addition that opens fastboot. Pinna then installs over
  fastboot. [firebreak](https://github.com/Gamer92000/firebreak) also unlocks these two. It runs the
  unlocks that Rortiz2 published on XDA, one thread for the
  [Echo Dot (2nd gen)](https://xdaforums.com/t/unlock-root-twrp-unbrick-amazon-echo-dot-2nd-gen-2016-biscuit.4761416/)
  and one for the
  [Echo (2nd gen)](https://xdaforums.com/t/unlock-root-twrp-unbrick-amazon-echo-2nd-gen-2017-radar.4801290/).
  Pinna needs Kaeru on these two; these docs don't say whether firebreak
  installs it.
- **Echo Dot (5th gen):** [firebreak](https://github.com/Gamer92000/firebreak)
  roots it over the USB test pads under its foot and installs a rooted Fire OS
  bundle, with rooted stock Fire OS in slot A and the TWRP recovery in slot B.
  [cannoli-os-builder](https://github.com/Gamer92000/cannoli-os-builder)
  builds that bundle. Pinna then installs from TWRP.

If yours is an Echo Dot (2nd gen) or an Echo (2nd gen), continue with
[What you need](/docs/requirements/). It lists the unlocked bootloader, the
computer and the backup that you need. These docs don't cover the unlock
itself. For the Echo Dot (5th gen), read the section below first.

### Echo Dot (5th gen)

The Echo Dot (5th gen) is a different board from the other two: another
processor, a 32-bit ARM kernel, and its system on NAND flash instead of eMMC.
Like the Echo (2nd gen), it has USB: through test
pads under its foot, which you solder to, as you do to the pins in the base of
the Echo (2nd gen).

How it is installed: firebreak unlocks it, which needs wires soldered to those
pads and a Fire OS version that firebreak supports, and Pinna is then written
into slot A from TWRP. The steps are on
[Install on the Echo Dot (5th gen)](/docs/flash/#install-on-the-echo-dot-5th-gen). Pinna doesn't
ship firebreak.

If the device doesn't start, there is a way back. TWRP stays in slot B: hold
**Volume Up** while you power it on. Pinna's rescue page works over USB.
firebreak's restore puts Fire OS back into slot A from the backup its unlock
makes with `--backup`: keep that backup. See
[The way back](/docs/flash/#the-way-back).

What works:

- Pinna boots from its own system.
- The ring works, and Home Assistant controls it.
- The buttons and the microphone switch are wired.
- The speaker plays cleanly. The volume follows the curve of Fire OS, the
  firmware the device ran before.
- The wake word and voice requests through Home Assistant work.
- All three microphones are captured, and so is the speaker's sound for the
  echo canceller.

Over-the-air updates aren't available on it yet. To install a newer Pinna,
see [Install a newer Pinna](/docs/flash/#install-a-newer-pinna).

## Other Echo devices

Pinna doesn't run on any device in this table yet, so you don't need to
unlock one now. A device needs two things first: a public way to unlock it,
and a port of Pinna to its hardware. The Echo Dot with clock (5th gen) and
the Echo Pop have the Echo Dot (5th gen)'s MT8519 processor, and the Echo
Dot with clock shares its device tree family, but Pinna doesn't run on
either.

The Echo Dot (3rd gen) is sold as two different boards: model D9N29T from
2018 and model C78MP8 from 2019. Only the D9N29T has a public unlock, so check
the model number on the label.

| Echo | Year | Model number | Codename | Chip | Bootloader unlock |
| --- | --- | --- | --- | --- | --- |
| Echo (1st gen) | 2014 | SK705DI | `doppler` | TI DM3725 | Debug pads |
| Echo Dot (1st gen) | 2016 | S04WQR | `pancake` | Not known | Not known |
| Echo Plus (1st gen) | 2017 | ZE39KL | `sonar` | Not known | None public |
| Echo Dot (3rd gen) | 2018 | D9N29T | `donut` | MediaTek MT8516 | Available |
| Echo Plus (2nd gen) | 2018 | L9D29R | `lidar` | MediaTek MT8516 | Available |
| Echo Input | 2018 | C1125P | `cupcake` | MediaTek MT8516 | Available |
| Echo Dot (3rd gen) | 2019 | C78MP8 | `crumpet` | MediaTek MT8516 | None public |
| Echo Dot with clock (3rd gen) | 2019 | 36EBT3 | `doebrite` | Not known | None public |
| Echo (3rd gen) | 2019 | R9P2A5 | `pascal` | MediaTek MT8516 | None public |
| Echo Studio (1st gen) | 2019 | O2T2V3 | `octave` | MediaTek MT8516 | None public |
| Echo Flex | 2019 | C77A68 | `croissant` | MediaTek MT8516 | None public |
| Echo Input Portable | 2019 | RA73E6 | `cupcake` | MediaTek MT8516 | Not tried |
| Echo (4th gen) | 2020 | L4S3RE | `laser` | MediaTek MT8512 | None public |
| Echo Dot (4th gen) | 2020 | B7W64E | `brownie` | MediaTek MT8512 | None public |
| Echo Dot with clock (4th gen) | 2020 | B7W644 | `ganache` | Not known | None public |
| Echo Dot with clock (5th gen) | 2022 | C4E8S3 | `cheesecake` | MediaTek MT8519 | None public |
| Echo Pop | 2023 | C2H4R9 | `churro` | MediaTek MT8519 | None public |
| Echo Dot Max | 2025 | MA49CA | `macaroon` | Amazon AZ3 | None public |
| Echo Studio (2nd gen) | 2025 | P8Z7MN | `persimmon` | Amazon AZ3 Pro | None public |

What the bootloader unlock column means:

- **Available**: a published tool unlocks the bootloader. On the D9N29T and
  the Echo Plus (2nd gen), you connect wires to test pads inside the device;
  the Echo Input unlocks over its micro-USB port.
- **Debug pads**: the device starts from an SD card wired to test pads on its
  base. This works on the earlier units; Amazon disabled the pads on the 2017
  edition.
- **None public**: nobody has published a way to unlock it.
- **Not tried**: the Echo Input Portable has the Echo Input's codename, but
  nobody has reported unlocking one.
- **Not known**: no information has been published.

Kids Editions aren't listed separately. The Echo Dot Kids Edition (4th gen),
for example, has the Echo Dot (4th gen)'s model number, B7W64E.

Some ports take more work than others:

- Pinna doesn't plan to support the Echo (1st gen). Its processor has one
  1 GHz core, which can't keep up with Pinna's wake word and echo canceller.
  Even while it waits for the wake word, Pinna uses about one core of the
  Echo Dot (2nd gen)'s faster processor.
- The MediaTek MT8516 models need board support in Pinna's kernel, and a
  driver for their Wi-Fi chip, the MT7668.
- The C78MP8 stores its system on raw NAND flash instead of eMMC. Pinna finds
  its partitions in a GPT partition table on eMMC, so this model needs a
  different storage layout as well as an unlock. The Echo Dot with clock
  (3rd gen), the Echo (3rd gen) and the Echo Flex run the same kernel and boot
  chain; whether they use NAND too hasn't been confirmed.

Unlock tools change as people publish new work. This table was checked on
21 November 2026. The models, codenames and chips come from the
[Amazon device codenames](https://bitbyte.miraheze.org/wiki/Amazon_device_codenames)
wiki and from echo-dot-assist's
[list of all Echos](https://github.com/Gamer92000/echo-dot-assist/blob/main/devices/MODELS.md).
If your model number isn't on this page, if you know of a change, or if you'd
like to help bring up a device you own, start a
[discussion](https://github.com/elsbrock/pinna/discussions).

## Not on this page

- Echo devices with a screen, such as the Echo Show, the Echo Spot and the
  Echo Hub. Pinna has no screen support.
- Echo devices that aren't smart speakers, such as the Echo Sub, the Echo
  Link and the Echo Auto.
