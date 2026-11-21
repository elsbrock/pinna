---
title: Echo devices
description: The Echo smart speakers Pinna runs on, and where every other Echo without a screen stands.
---

Pinna runs on two Echo devices. This page lists every Echo smart speaker
without a screen, so you can check whether yours runs Pinna before you get
anything else ready.

To find your Echo, look for the model number on the label on its base. The
codename is the device's internal name. Fastboot reports it in capitals, for
example `BISCUIT`, which helps when a label is worn off.

## Supported

| Echo | Year | Model number | Codename | Chip | Bootloader unlock |
| --- | --- | --- | --- | --- | --- |
| Echo Dot (2nd gen) | 2016 | RS03QR | `biscuit` | MediaTek MT8163 | amonet, with Kaeru |
| Echo (2nd gen) | 2017 | XC56PY | `radar` | MediaTek MT8163 | amonet, with Kaeru |

If yours is one of these, continue with [What you need](/docs/requirements/).
It lists the unlocked bootloader, the computer and the backup that you need.
These docs don't cover the unlock itself.

## Other Echo devices

Pinna doesn't run on any device in this table yet, so you don't need to
unlock one now. A device needs two things first: a public way to unlock it,
and a port of Pinna to its hardware.

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
| Echo Dot (5th gen) | 2022 | C2N6L4 | `cannoli` | MediaTek MT8519 | By hand |
| Echo Dot with clock (5th gen) | 2022 | C4E8S3 | `cheesecake` | Not known | None public |
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
- **By hand**: the unlock works, but there's no tool for it. On the Echo Dot
  (5th gen) it needs soldered wires and one particular Fire OS version.
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
