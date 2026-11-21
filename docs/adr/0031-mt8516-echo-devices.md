# ADR 0031: The MT8516 Echos: a 64-bit kernel under the same root

Status: proposed (2026-11-21), accepted (2026-11-21). Built in part
(2026-11-21): the MT8516 kernel with the Echo Dot (3rd gen) board and
MT7668 Wi-Fi, LK's 64-bit boot image layout, the Wi-Fi firmware in the
root image, and a check under QEMU that
the armhf root runs on an arm64 kernel. Nothing has run on an MT8516
device. Amends ADR 0001 (one architecture, armhf) and ADR 0006 (one board
family in the kernel). The evidence below comes from TECHO5,
lidar-mainline, Amazon's kernel sources and the Linux tree. The board
layer this port needs is ADR 0032.

## Context

Pinna runs on the MT8163 Echos: the Echo Dot (2nd gen) and the Echo (2nd
gen), 32-bit ARM, Linux 6.12 with `kernel/patches` (ADR 0006). "More Echo
devices" is listed as later work in `PRODUCT.md`. The owner ruled out the
devices with a screen until Pinna has screen support (2026-11-21).

The Echos without a screen that have a public unlock and a chip Pinna could
support are the three MediaTek MT8516 models (`website/src/content/docs/devices.md`):

| Echo | Model number | Codename | Unlock |
| --- | --- | --- | --- |
| Echo Dot (3rd gen) | D9N29T only | `donut` | kamakiri-donut, wires to test pads |
| Echo Input | C1125P | `cupcake` | kamakiri-cupcake, over micro-USB |
| Echo Plus (2nd gen) | L9D29R | `lidar` | davidwatters/echo-plus-2-unlock, wires to test pads |

The Echo (3rd gen) is covered at the end. The owner ordered an Echo Dot
(3rd gen) and a used Echo (3rd gen) on 2026-11-21. Whether the Dot is a
D9N29T isn't known yet.

### Sources

- **TECHO5** (MIT, Go): runs the Echo Dot (3rd gen) and the Echo Input on
  Amazon's own 64-bit 4.4.22 kernel. Its kernel work doesn't carry over to
  6.12; its hardware facts do. Paths below are in
  `HuskerMinion/techo5-dot` (commit 7b5ed24) and `HuskerMinion/techo5`
  (`echod/internal`, commit 647f4aa). TECHO5 has nothing on the Echo Plus
  (2nd gen).
- **LeagueRaINi/lidar-mainline** (GPL-2.0, commit db8242a, 2026-11-20):
  mainline Linux 7.3 and U-Boot for the Echo Plus (2nd gen). 50 commits in
  five days from one contributor; nothing posted upstream yet. Its README
  reports the speaker, the line-out, all 7 microphones, the echo reference,
  the LEDs, buttons and privacy switch, cpufreq, eMMC HS200, Wi-Fi,
  Bluetooth (on a second unit too) and pstore as working; suspend and deep
  idle as not working. These are its own reports, not measured here.
- **Amazon's kernel sources** for each Echo, from Amazon's "Source Code
  Notice for Echo and Alexa Devices"
  (`https://fireos-audio-src.s3.amazonaws.com/3kHx7XdMvEyjQugkSg3dXgwmva/index.html`).
  The 6.5.7.1 releases (October 2025) for the Echo Dot (3rd gen), the Echo
  Input and the Echo Plus (2nd gen) are one arm64 Linux 4.4.22 tree,
  `kernel/mediatek/mt8167/4.4`, built with `donut_defconfig`,
  `cupcake_defconfig` and `mt8167_defconfig`. They differ only in their
  board files. Beside the kernel is the MT7668 Wi-Fi driver,
  `vendor/amazon/wlan/mediatek/driver/mt76x8`, with a configuration for
  each board. GPL-2.0. Kernel paths below are in this tree.
- **Linux v6.12.117**, the tree Pinna builds (ADR 0006), and upstream v7.3
  and v7.4-rc3 for what landed later.
- echo-dot-assist (`devices/MODELS.md`, commit 1d7d957) and the bitbyte
  Amazon device codenames wiki for models and unlocks.

### The boards

| | Echo Dot (3rd gen) | Echo Input | Echo Plus (2nd gen) | Echo Dot (2nd gen), today |
| --- | --- | --- | --- | --- |
| SoC | MT8516, 4x Cortex-A35 | same | same | MT8163, 4x Cortex-A53 |
| RAM | 512 MB | not stated | 1 GiB | ~480 MB |
| Storage | 4 GB eMMC, GPT | same as donut | eMMC | eMMC, GPT |
| Speaker | TI TAS2770 on I2S | none | TLV320AIC32x4 DAC, GPIO amplifier | TLV320AIC32x4 |
| 3.5 mm | MT8167 internal codec | MT8167 internal codec, line out only | through the AIC32x4 | none |
| Microphones | 4, 2x TLV320ADC3101, TDM | same as donut | 7, 4x TLV320ADC3101, TDM | 7, TLV320ADC3101 behind an SPI FPGA |
| Echo reference | AFE loopback (AWB) | same | AFE loopback | 2 lanes in the capture PCM |
| Light | IS31FL3236, 12 RGB | one RGB LED, TI LP5562 | IS31FL3236A ring | IS31FL3236, 12 RGB |
| Buttons | Vol+, Vol-, Action, Mute | Action, Mute | Vol+, Vol-, Action, Mute | Vol+, Vol-, Action, Mute |
| Wi-Fi and Bluetooth | MT7668, SDIO | same | MT7668, SDIO | MT8163 CONSYS |
| Power | Fixed regulators, Richtek RT5738 for the CPU, no PMIC | same | same | MT6323 PMIC |

Sources: `techo5-dot/docs/hardware-donut.md:24-43`,
`docs/hardware-cupcake.md:10-44`, `techo5/echod/internal/hardware/speaker/paths_donut.go:38-47`,
`hardware/mic/device_donut.go:9-16`, `hardware/led/led.go:20-29`,
`hardware/buttons/codes_dot.go:18-35`; lidar-mainline
`lidar-linux/arch/arm64/boot/dts/mediatek/mt8516-amazon-lidar.dts`;
Amazon's `donut.dtsi` and `cupcake.dtsi`, which include `mt8516-dcdc.dtsi`
and put the RT5738 at `0x57`.

### Unlock and boot chain

- **Echo Dot (3rd gen).** kamakiri-donut is a BootROM exploit: hold Action
  while power is applied, with USB wired to test pads TM4 (D−) and TM5
  (D+); the case has no USB port (`hardware-donut.md:35,106-113`). It
  leaves Kaeru in the bootloader and TWRP in `swdl`.
  - Kaeru boots `boot_a` or `boot_b` and passes `androidboot.slot_suffix`
    (`tools/linux/rootfs-donut/etc/techo5/boot.sh:43-46`).
  - It boots `swdl` as recovery when `misc` holds `boot-recovery`; there is
    no `recovery` partition (`hardware-donut.md:39-47`,
    `tools/linux/init-donut:46-70`).
  - Partitions: `misc`, `boot_a`, `boot_b`, `swdl` (32 MB), `system_a`,
    `system_b`, `cache` (32 MB), `userdata` (1.5 GB)
    (`hardware-donut.md:41-43`). These are the names Pinna uses: pinna-init
    finds `misc`, `system_a` and `system_b`, the updater writes `boot_a`
    and `boot_b`, and `/var` is `userdata`.
  - The boot image is Android v0 (`tools/linux/mkimage.py:107-113`). The
    stock ramdisk address leaves too little room below the crash log
    areas from `0x44400000` (`ram_console`, then pstore at `0x44410000`);
    TECHO5 moves it to `0x43478000`
    (`tools/dotimage.py:82-84`), the address Pinna's boot image already uses.
  - What Kaeru's fastboot allows on this model isn't recorded. Failed
    unlocks report "the command you input is restricted on locked hw", and
    two units were left unusable (echo-dot-assist
    `devices/donut/README.md:34-40`). TECHO5 installs from TWRP over adb,
    not with `fastboot flash`.
- **Echo Input.** kamakiri-cupcake runs over the micro-USB socket and
  leaves TWRP in `swdl`; otherwise the same chain as the Dot
  (`hardware-cupcake.md:61-73`). Fire OS 6.5.7.5 first: on 6.5.3 the Wi-Fi
  firmware halts after about two minutes (`hardware-cupcake.md:83-87`).
- **Echo Plus (2nd gen).** BootROM, preloader, BL31, LK with Kaeru, then
  `boot_a` or `boot_b`. Kaeru's fastboot opens when Action is held at
  power-on. lidar-mainline boots U-Boot from the boot slot, or Linux
  directly (README, "Boot chain"). echo-dot-assist's model list still says
  "no known exploit" for this model (`devices/MODELS.md:14`); the unlock
  lidar-mainline points to is davidwatters/echo-plus-2-unlock.
- **What LK wants from a 64-bit boot image** (lidar-mainline README, "What
  LK requires of a boot image", and `tools/lidarboot.py:23-36`):
  - Android boot image v0, page size 2048.
  - The kernel is a 512-byte MediaTek `KERNEL` header (magic `0x58881688`)
    over gzip(arm64 `Image`), with the device trees appended.
  - LK picks the appended device tree whose first root `compatible` matches
    the board exactly (`amzn,lidar-pvt` on a retail Echo Plus).
  - The command line starts with `bootopt=64S3,32N2,64N2`, or LK doesn't
    take the 64-bit path.
  - Entry at `0x40080000`; the image and its BSS end below BL31 at
    `0x43000000`.
  - A ramdisk is required.
  - Pinna's `pinna-bootimg` wrote only `bootopt=64S3,32N2,32N2` and a
    zImage with one device tree, so it needed a second layout (now
    `pinna_bootimg::arm64`).
  - Amazon's own boot image for the Echo Dot (3rd gen), in its Fire OS
    6574.1 update, has this layout: kernel at `0x40080000`, tags at
    `0x4E000000`, `bootopt=64S3,32N2,64N2`, and five appended device trees,
    `amzn,donut-proto`, `-evt`, `-dvt`, `-hvt` and `-pvt`, so LK picks
    the tree by board revision. A 4 KiB verified-boot signature follows
    the image.

### The userland can stay 32-bit

TECHO5 runs a 32-bit (armv7) userland on the Echo Dot (3rd gen)'s 64-bit
kernel, as Fire OS does (`hardware-donut.md:25`). The Cortex-A35 runs
AArch32 code at EL0, and an arm64 kernel with `CONFIG_COMPAT` runs it. Pinna's
root image is armhf Debian with static musl binaries for armv7
(`.cargo/config.toml`, `mkosi/mkosi.conf`); it contains no kernel modules,
because the kernel is monolithic (ADR 0006). The kernel package in the root
is unused at boot: the boot image carries the kernel.

### What mainline 6.12 has

Verified in the v6.12.117 tree:

- **There:** `mt8516.dtsi` (v5.7, 5236347bde42) and `mt8167.dtsi` (v5.11):
  clocks (`clk-mt8516*`, v5.2), pinctrl, the PMIC wrapper, eMMC (mtk-sd),
  I2C, SPI, MUSB and its PHY, watchdog, auxadc, efuse, PWM, cpufreq. The
  December 2024 fixes (BL31 reservation, GIC range, watchdog IRQ) are in.
  - `btmtksdio` drives the MT7668's Bluetooth since v5.2 (9aebfd4a2200).
  - The codec and LED drivers: `tas2770`, `tlv320adc3xxx`, `tlv320aic32x4`,
    `lp5562`, `is31fl32xx` (IS31FL3236 only: the 3236A came in v6.18,
    758e743362cd), and `tsl2772`, which covers the TSL2572 light sensor.
- **Not there, in 6.12 nor in v7.4-rc3:**
  - an audio front end (AFE) for MT8516 or MT8167;
  - a driver for the MT8167 internal codec (the 3.5 mm socket);
  - a driver for the MT6392 PMIC. None of the three boards has one (see
    the table), so none needs it;
  - SoC thermal for MT8516;
  - MT7668 Wi-Fi. mt76's SDIO driver matches only the MT7663S
    (`037a:7603`); the MT7668's Wi-Fi function is `037a:7608`.

### What Amazon's 4.4 tree has for the gaps

Every gap has a driver in Amazon's tree. Each is written against the 4.4
APIs: the codec drivers use `snd_soc_codec`, which current ASoC no longer
has. Using one on 6.12 is a port, not a copy.

| Gap in 6.12 | In Amazon's tree | Lines |
| --- | --- | --- |
| AFE | `sound/soc/mediatek/mt8167/mt8167-afe-*.c` | 6,533 |
| MT8167 internal codec | `sound/soc/codecs/mt8167-codec.c`, `mt8167-codec-utils.c` | 3,868 |
| Machine drivers | `sound/soc/mediatek/mt8167/donut.c`, `cupcake.c`, `lidar.c` | 578, 529, 770 |
| SoC thermal | `drivers/misc/mediatek/thermal/mt8167/` | 7,005 |
| MT7668 Wi-Fi | `vendor/amazon/wlan/mediatek/driver/mt76x8/` | 93 C files; 230,161 with headers |

The machine drivers record each board's audio wiring:

- On the Dot, DL1 goes to the TAS2770 over I2S, the two ADC3101s come in on
  `TDM_IN`, and AWB is the loopback.
- On the Dot and the Input, `INT ADDA` goes to the internal codec.
  The internal codec reaches only the AFE's and APMIXEDSYS's registers
  (`mt8167-codec.c:30-34`), not the PMIC.
  - `cupcake.c:309-312` links `INT ADDA` to `mt8167-codec`.
  - `donut.c:314-316` does the same.

lidar-mainline's AFE is a smaller port of Amazon's
(`mt8516-afe-pcm.c:7`). It has no `INT ADDA` interface, and it routes the
ADC input to the external codecs (`mt8516-afe-pcm.c:350-351`).

### What lidar-mainline adds, and what it takes to bring to 6.12

| Part | Size | On 6.12 |
| --- | --- | --- |
| `mt8516.dtsi`: AFE node, reset cells, MSDC2 clock fix | +77 −2 | Applies; the MSDC2 bug is in 6.12 too |
| `clk-mtk.c`, `clk-mt8516.c`: unreachable clock ID 176, reset bank | +44 −1 | Applies |
| AFE driver (`sound/soc/mediatek/mt8516/`): DL1 to I2S, 8-channel TDM in, AWB loopback, no internal codec | ~1,600 lines, new | No missing API found |
| Machine driver `mt8516-amazon-lidar.c` | 571 lines, new | Two helpers from v6.18 to replace |
| `tlv320adc3xxx`: TDM slots, BCLK, shared reset | +193 −37 | Easy |
| `tlv320aic32x4`: biquads, mono mix | +220 −74 | Rework: built on the v7.3 refactor; Pinna's patch 0014 changes the same file |
| `btmtksdio`: MT7668 LE command bits | +32 | Applies |
| MT7668 Wi-Fi (`lidar-wifi/`), from Amazon's `mt76x8` driver, GPL-2.0 OR BSD-3-Clause | 175 files, 216,319 lines | Hard: written for 7.3 without version guards; it uses cfg80211 and timer APIs that changed in 6.14, 6.16, 6.17, 7.1 and 7.3 |
| Board device tree, audio include, 5 bindings | ~750 lines, new | New files |

The Wi-Fi port is the same kind of work as Pinna's MT8163 CONSYS driver
(214k lines of vendor code, patches 0018 to 0031). It needs Amazon's
firmware, plus `mt7668pr2h.bin` from linux-firmware (redistributable) for
Bluetooth and as the Wi-Fi driver's ROM patch. TECHO5 copies Amazon's files
from each unit (`tools/linux/rootfs-donut/etc/techo5/boot.sh:146-189`).
They are also in Amazon's own update for the Echo Dot (3rd gen): Fire OS
6574.1 for `donut_puffin`, on Amazon's update server
(`update-kindle-donut_puffin-NS65741_user_8138_0013222529668.bin`, sha256
`ac22b78c…a0245`, the file echo-dot-assist pins), in `/vendor/firmware` of
its system image: `WIFI_RAM_CODE_MT7668.bin`,
`WIFI_RAM_CODE2_SDIO_MT7668.bin`, `TxPwrLimit_MT76x8.dat`, `wifi.cfg`,
`EEPROM_MT7668.bin` and `mt7668_patch_e2_hdr.bin`. The system image is the
same on every unit, so the files are too. As ADR 0001 decides for the
MT8163's, they are fetched from `https://static.iodev.org/pinna/firmware/donut/fireos-6574.1-8138/`
and not committed. The root image installs them, Amazon's `wifi.cfg` as
`wifi_mt7668.cfg`: the MT8163's CONSYS driver reads `/etc/firmware/wifi.cfg`,
and `/etc/firmware` is `/usr/lib/firmware`, so patch 0112 renames the
MT7668's. linux-firmware's `mt7668pr2h.bin`, fetched from kernel.org at a
release tag, goes in as `mediatek/mt7668pr2h.bin` for Bluetooth and as
`mt7668_patch_e2_hdr.bin` for Wi-Fi. Amazon's ROM patch is a different build from
linux-firmware's, and lidar-mainline found it crashes the Bluetooth firmware
under `btmtksdio`. It stays on `static.iodev.org` for reference but isn't
fetched, because the Wi-Fi driver would load it under its name.

### What each board needs beyond that

- **Echo Dot (3rd gen):**
  - A machine driver for the TAS2770 and two ADC3101s. lidar-mainline's AFE
    has the three paths it needs: I2S out, TDM in, loopback. Amazon's
    `donut.c` has the links and formats.
  - Its own device tree, from Amazon's `donut.dtsi` and `donut-pvt.dts`.
    LK picks it by the first compatible string, which is in the stock boot
    image's device trees.
  - The ring: an IS31FL3236 at `0x3F` with channel offset 3, as on the
    Echo Dot (2nd gen), so Pinna's ring patch (0011) serves it. Amazon's
    `donut.dtsi` also lists an IS31FL3208 at `0x6F`, but `donut-pvt.dts`,
    the retail board, deletes it.
  - The speaker: TECHO5 plays at 96 kHz S16_LE, because the TAS2770
    produced static at 48 kHz on the stock kernel (`paths_donut.go:247-287`,
    techo5-dot#20). Whether that holds on 6.12 with the mainline TAS2770
    driver is to be measured.
  - The microphones: 4 lanes, S32_LE at 16 kHz, and the reference on a
    second PCM (`device_donut.go:9-16`). Which lane is which microphone, and
    where the microphones sit, aren't known (`device_donut.go:18,35-36`).
  - The microphone switch is Amazon's `gpio-privacy`
    (`hardware/privacy/platform_donut.go:10-50`), not the MT8163's
    `amz_privacy` (patch 0012).
  - The 3.5 mm socket needs the MT8167 internal codec. It can wait for the
    Echo Input's port.
- **Echo Input:** everything the Dot needs, except the speaker, plus:
  - **the MT8167 internal codec: its only audio output is the line out.**
    Neither mainline nor lidar-mainline has a driver; Amazon's tree does
    (see above).
    - The port moves it to the component API and adds `INT ADDA` to
      lidar-mainline's AFE.
    - Until that is done, the Echo Input can't reply.
  - a ring driver for one RGB LED (`led.go:23-29`, the LP5562), and no volume
    keys.
- **Echo Plus (2nd gen):** lidar-mainline's board as it is, on 6.12: the
  AIC32x4 rework and the IS31FL3236A support from v6.18. Its 7-microphone
  array is the closest to Pinna's beamformer.

### The Echo (3rd gen), and the Echo Dot (3rd gen) C78MP8

Both are `crumpet`-class: MT8516 on raw NAND, without eMMC or GPT, with a
32-bit kernel shared with the Echo Dot with clock (3rd gen) and the Echo
Flex. Neither has a public unlock (echo-dot-assist `devices/MODELS.md:20`,
issue #178). Community work in echo-dot-assist #135 got root on a C78MP8
through older preloaders; a 2023 preloader may close the BootROM, and a used
unit has probably received it. Even unlocked, pinna-init's GPT lookup and the
slot layout don't apply as they are.

Amazon's releases for them don't describe the hardware.

- The Echo (3rd gen)'s release (`Echo_v3_AVS_src`) is the same 4.4 tree.
  Its Kconfig names an "MT8516 Pascal board" machine driver
  (`SND_SOC_MT8516_PASCAL_MACH`), but `pascal.c` and a `pascal` device tree
  aren't in it. Its device trees are the `octave` and `megalo` boards.
- The C78MP8 Dot's release (`Echo_Dot_3rd_Gen_AVS_src`) has the `lidar`,
  `octave` and `megalo` boards, and no `donut`.

## Decision

1. **The MT8516 is Pinna's second SoC.** One port for the three boards,
   with one kernel and one boot image layout. The boards differ in their
   device tree, their machine driver and their board description (ADR 0032).
2. **A 64-bit kernel under the same 32-bit root.** The MT8516 boards boot an
   arm64 kernel with `CONFIG_COMPAT` and the armhf root image every
   satellite runs. Releases ship one root image and one boot image per SoC.
   No arm64 userland: no second Rust target, no second mkosi image, no
   second set of models, and the update channels stay as they are. A 64-bit
   userland is a separate decision, taken only if a measured need comes up.
3. **Debian's 6.12, as ADR 0006 decides.** lidar-mainline's MT8516 work is
   backported to 6.12 as a series of its own, credited to it, with Pinna's
   fixes after it, as was done with LibreEcho's MT8163 work.
   - `kernel/build.sh` gains two arm64 variants: `mt8516` for the Echos,
     and `qemu-arm64`, which `just vm-test-arm64` boots with the armhf
     image under QEMU's Cortex-A53.
   - `device.config` stays the MT8163's; `mt8516.config` is the MT8516's.
     The MT8516 patches are numbered from 0101. Every variant applies
     every patch.
   - The MT7668 Wi-Fi driver is built in, like CONSYS. It keeps
     lidar-mainline's 7.3 signatures; version guards adapt them to 6.12.
     Built in, it probes before the root that holds its firmware is
     mounted, so it is to be bound again once the root is up; that isn't
     built yet.
   - Moving the MT8516 to a newer kernel than the MT8163 is the fallback if
     the backport costs more than it saves; that would need its own ADR.
4. **Which device first: the Echo Dot (3rd gen) D9N29T, when one is at
   hand.** It is the common one, it has a speaker, and every part it needs is
   in 6.12 or in lidar-mainline except its machine driver, its device
   tree, the Wi-Fi backport and the microphone switch driver
   (lidar-mainline reads its latch through `gpio-keys`). Amazon's `donut.c`
   and `donut.dtsi` show the first two, and its `gpio_privacy.c` is the
   switch driver's source. The Echo Plus (2nd gen) is the reference: if one is at hand
   first, it goes first, because lidar-mainline already runs on it.
5. **The Echo Input comes after the Dot (3rd gen).** It needs one more
   driver: the MT8167 internal codec, ported from Amazon's tree. The same
   port gives the Dot its 3.5 mm socket.
6. **Not now:** the Echo (3rd gen) and the C78MP8 Echo Dot (3rd gen): no
   public unlock, and NAND. They stay research.
7. **Nothing is written to a device before the owner has one and agrees.**
   The first device steps, in order, are:
   - check the label says D9N29T;
   - back up the whole eMMC under its current firmware;
   - read the partition table and its sizes, the device trees in the stock
     boot image, the idme record in LK's device tree, and Kaeru's slot record
     in `misc`;
   - test what Kaeru's fastboot allows (`flash`, `boot`, `oem`).

## How the design keeps the goals

- **Be safe to install and update:** an MT8516 satellite keeps A/B slots
  and a fastboot that Pinna never writes. Before the first boot image is
  written, it is shown that Kaeru boots by its slot record and opens fastboot
  from `misc`, as ADR 0019 measured on the Echo Dot (2nd gen). Until then the
  port isn't offered to owners. The rescue system goes in `swdl` where there
  is no `recovery`.
- **Use standard parts:** the kernel parts come from mainline or go there.
  lidar-mainline's series is drafted for upstream, and the internal codec
  port is written the same way. The Wi-Fi driver is the exception, as
  CONSYS is on the MT8163.
- **Stay open:** Amazon's Wi-Fi firmware and speaker tuning are not
  committed. The firmware is fetched from `static.iodev.org`, as the
  MT8163's is (ADR 0001).
- **Make development pleasant:** one root image and one Rust toolchain for
  every satellite; the kernel build adds an aarch64 cross compiler. The virtual Echo Dot and its tests keep covering the userland
  all boards share.

## Consequences

- `pinna-bootimg` gains the MediaTek `KERNEL` layout: gzip(Image), several
  device trees appended, `bootopt=64S3,32N2,64N2`, entry at `0x40080000`. The
  browser installer (ADR 0016) needs the new fastboot products and partition
  checks.
- `pinna-init`, `pinna-bootctl` and the updater find the same partition
  names. Kaeru's slot record on the MT8516 needs to be measured before they
  write it.
- `pinna-hal` and `pinna-audio` take each board's facts from its
  description (ADR 0032):
  - an LED driver for the Echo Input's LP5562. The Dot's IS31FL3236 has
    the address, channel offset and frame interface of the Echo Dot (2nd
    gen)'s (patch 0011), so the existing driver serves it;
  - the `gpio-privacy` switch, whose files are named `state` and `enable`
    where `amz_privacy` has `privacy_state`, `privacy_trigger` and
    `mute_lamp`;
  - a capture with four S32 lanes and the reference on a second PCM;
  - a beamformer for four microphones whose layout is to be measured;
  - the TAS2770 mixer sequences.
- The beamformer, the echo canceller and the wake word thresholds were
  tuned on the MT8163 array. They need measurements on each new array before
  owners use it.
- The kernel grows from one board family to two. Each needs a device to
  test, and no CI runs device tests (AGENTS.md).
- Until a D9N29T or an Echo Plus (2nd gen) is at hand, the work that can
  proceed is the board layer (ADR 0032), the boot image layout and the
  arm64 kernel build. The last two are built, and the root image carries
  the Wi-Fi firmware. Binding the Wi-Fi after the root, a boot image in
  releases and the board's description are not.
