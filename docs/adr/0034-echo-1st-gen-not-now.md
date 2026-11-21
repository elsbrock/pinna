# ADR 0034: The Echo (1st gen): not now

Status: proposed (2026-11-21). The owner decided to shelve the port
(2026-11-21), after the measurements below. Nothing was built and nothing
was written to a device.

## Context

The website listed the Echo (1st gen) as "in the works" (status page,
landing page, Echo devices page) from 2026-11-20, without a record behind
it. The owner has a unit from 2015 or 2016, the editions that boot from an
SD card on the base pads. This record collects what a port would take, so
that the decision and its reasons are kept.

### Sources

- **Linux v6.12.117**, the tree Pinna builds (ADR 0006):
  `arch/arm/boot/dts/ti/omap/omap3-echo.dts`, upstreamed by André Hentschel,
  with the speaker, Wi-Fi, the LEDs and the controls.
- **u-boot-echo** (`gitlab.com/AndreRH/u-boot-echo`, branch `echo`): U-Boot
  2010.06 with Amazon's board code, imported from Amazon's source release
  `Echo_src_5.5.3.7_build653619520`. Amazon's current source page
  (ADR 0031) has no archive for this model; the oldest is the Echo (2nd gen).
- **EchoRoot** (`gitlab.com/AndreRH/echoroot`, GPL-2.0, last commit
  fb3e7664, 2021-11-28): the stock 2.6.37 kernel with a Buildroot userland and
  Porcupine, and its project page, `andrerh.gitlab.io/echoroot`.
- **Security research:** Clinton, Cook and Banik, "A Survey of Various Methods
  for Analyzing the Amazon Echo" (2016), and MWR Labs, "Alexa, are you
  listening?" (Mark Barnes, 2017).
- **iFixit's Amazon Echo teardown** for the chips.
- **Prometheus** on the owner's two satellites, 2026-11-21, for Pinna's load.

### The board

| | Echo (1st gen) | Echo Dot (2nd gen), today |
| --- | --- | --- |
| SoC | TI DM3725: one Cortex-A8 at 1 GHz, NEON, C64x+ DSP | MT8163, 4x Cortex-A53 |
| RAM | 256 MB LPDDR1; 198 MB in the mainline device tree, `mem=200M` from the stock U-Boot | ~480 MB |
| Storage | 4 GB SanDisk iNAND, GPT | eMMC, GPT |
| Speaker | TLV320DAC3203 on McBSP2, TPA3110D2, woofer and tweeter | TLV320AIC32x4 |
| Microphones | 7, on 4x TLV320ADC3101 | 7, TLV320ADC3101 behind an SPI FPGA |
| Light | 4x LP5523, 12 RGB LEDs | IS31FL3236, 12 RGB |
| Controls | Volume ring (rotary encoder), Action (`KEY_HELP`), mute (`KEY_MUTE`) | Vol+, Vol-, Action, Mute |
| Wi-Fi and Bluetooth | QCA6234X module: AR6004 on SDIO (`ath6kl`), AR3002 on a UART | MT8163 CONSYS |
| Power | TPS65910 PMIC | MT6323 PMIC |
| USB | None in the mainline device tree; the base pads carry a UART and the SD lines | Micro-USB (the Echo (2nd gen): pogo pins) |

Sources: iFixit for the chips and RAM; `omap3-echo.dts` for the buses,
addresses, keys, the 198 MB memory node and the missing microphones (the
device tree has no node for the ADC3101s); u-boot-echo
`include/configs/omap3_lab126.h` for `mem=200M@0x80000000` and
`mpurate=1000`. The Wi-Fi and Bluetooth wiring is the stock firmware's
(`ath6kl_sdio` with `ath6k/AR6004/hw3.0/bdata.bin`, `hciattach /dev/ttyO2
ath3k`), from EchoRoot's `rootfs/additional/S30misto`. linux-firmware has no
`AR6004/hw3.0` files, so the Wi-Fi firmware would come from the stock image.

### Boot chain and storage

- **The SD card route.** The base has 18 pads: power, UART (115200 8N1,
  1.8 V) and the SD lines. The ROM tries the SD card before the eMMC, so a
  card wired to the pads boots (Clinton et al., MWR). On the 2017 edition
  Amazon tied the 3 V pad to the SD command line, which closes the route; its
  part number ends in -02 (MWR).
- **Stock boot chain:** X-Loader 1.51, U-Boot 2010.06, Linux 2.6.37. U-Boot
  has `CONFIG_BOOTDELAY 0` and `CONFIG_ENV_IS_NOWHERE`, so its console can't
  be reached from the eMMC; a U-Boot started from the SD card can be.
  `CONFIG_OF_LIBFDT` is off: it passes no device tree to the kernel.
- **Partitions** (`board/ti/lab126/partition.c`): `xloader` (896 KiB raw),
  `recovery` (16 MiB FAT), `boot` (16 MiB FAT), `idme` (16 MiB), `diags`
  (128 MiB), `main-A` and `main-B` (512 MiB each, ext3), `data` (the rest).
  None of them has the names pinna-init and the updater use.
- **Slots:** U-Boot picks `main-A` or `main-B` by the idme variables
  `bootmode`, `knowngood` and `otaretrycount`, counts tries and falls back to
  the known good one (`board/ti/lab126/evm.c`, `prepare_eMMC_boot`). It loads
  the kernel from `/boot/uImage` inside the slot's ext3 file system
  (`ext4load`), so an erofs root (ADR 0001) can't hold it.

### What mainline 6.12 has

Every peripheral in the table has a mainline driver, and the device tree
wires all of them except the microphones: `omap_hsmmc`, `ath6kl`,
`leds-lp5523`, `tlv320aic32x4` with McBSP and `simple-audio-amplifier`,
`rotary-encoder`, `gpio-keys` and `tps65910`. `tlv320adc3xxx` drives the
ADC3101s (ADR 0031), but how they reach the SoC isn't published. OMAP3 is in
`multi_v7_defconfig` and isn't marked for removal.

### Pinna's load against the CPU

Measured on 2026-11-21 over one hour at idle, from
`pinna_unit_cpu_seconds_total` and the memory gauges (`/metrics`), on the
builds the owner's satellites ran:

| | Echo Dot (2nd gen) | Echo (2nd gen) |
| --- | --- | --- |
| pinnad | 0.72 cores | 0.72 cores |
| PipeWire (beamformer, WebRTC echo canceller) | 0.37 cores | 0.25 cores |
| All services | 1.10 cores | 0.99 cores |
| pinnad's memory (cgroup) | 83 MB | 151 MB |
| Every other service | 68 MB | 55 MB |
| In use (`MemTotal` − `MemAvailable`) | 149 MB | 214 MB |

Cores are Cortex-A53 cores at the MT8163's clock.

- **The CPU.** At idle Pinna needs about one A53 core, before a reply, a media
  stream or Wi-Fi traffic. The DM3725 has one Cortex-A8 at 1 GHz.
  - ARM's Dhrystone figures (2.0 DMIPS/MHz for the A8, 2.3 for the A53) put
    it below one A53 core.
  - Its VFP unit (VFPLite) isn't pipelined, so scalar floating point is
    slower still.
  - EchoRoot found the device "by far too slow" for ODAS, an open microphone
    array pipeline. The stock firmware does that work on the C64x+ DSP, whose
    drivers end at Linux 2.6.x ("at best in a broken state until 3.16"), and
    whose code is proprietary (EchoRoot project page). Mainline has no DSP
    loader for OMAP3: `omap_remoteproc` covers OMAP4, OMAP5 and DRA7.
- **The memory.** 198 MB, or 256 MB if the stock limit turns out not to be
  needed, against 149 to 214 MB in use. pinnad's share is mostly retained by
  musl's allocator, not live data.
- **The binaries.** `.cargo/config.toml` builds every device binary with
  `target-cpu=cortex-a53`, which lets LLVM use instructions the A8 doesn't
  have, such as hardware divide and VFPv4's fused multiply-add. The root image every satellite
  shares (ADR 0031) doesn't run on it as built. Building for an ARMv7
  baseline would take those instructions from the MT8163 and MT8516 too.

## Decision

1. **Pinna isn't ported to the Echo (1st gen) now.** Pinna's audio chain
   needs more processor than the DM3725 has. Fitting it would mean a lighter
   chain for this model alone, an ARMv7 build of the root image, a boot
   chain and slot layout of its own (a U-Boot that Pinna builds, a kernel
   outside the root, other partition names), and install and recovery
   through wires on the base pads only, with no USB.
2. **The website stops saying it is in the works.** The Echo devices page
   keeps the model in its table and says why Pinna doesn't plan to support
   it: the processor.
3. **The owner's unit stays as it is.** Nothing is written to it.
4. **What reopens this:** a new record, if Pinna's whole idle load is
   measured on a Cortex-A8 and fits, with room for a reply and a media
   stream, for example after a lighter audio chain that other devices want.

## How the design keeps the goals

- **Be a good Home Assistant voice satellite:** a satellite whose wake word
  and echo canceller can't keep up with its microphones would miss wake words
  and talk over itself. Shelving the port keeps the promise for the models
  Pinna supports, instead of adding one that can't keep it.
- **Be safe to install and update:** without USB, a failed install or update
  on this model is undone only by wiring an SD card to the base pads, and the
  2017 edition needs a board rework before even that. Pinna doesn't offer a device it can't get
  back.
- **Make development pleasant:** one root image and one Rust target stay
  as they are, without a slower ARMv7 baseline for every satellite.

## Consequences

- `devices.md` says the Echo (1st gen) isn't planned, and why. The landing
  page's generation timeline marks it "Not planned", and the status page no
  longer lists it under Planned.
- The research stays here for a later attempt. A port would start from the
  mainline device tree; add the microphones to it (the ADC3101s' bus and
  addresses, measured on a unit); take the AR6004 firmware from the stock
  image; and either build U-Boot for Pinna's slots or start the 6.12 kernel
  with an appended device tree from the stock U-Boot, whose kernel must then
  live outside the erofs root.
- The questions only a unit answers stay open: the AR6004 revision and where
  its MAC and calibration live, the microphones' wiring, how the mute state
  reads, whether all 256 MB work under mainline, and the boot straps.
