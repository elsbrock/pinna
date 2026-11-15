# ADR 0006: Kernel — Debian's 6.12 with the MT8163 port, minimal config

Status: accepted (2026-11-15). Supersedes the kernel part of ADR 0001.

## Context

- ADR 0001 built LibreEcho-Linux release/0.14.5: Linux 6.1.178 with the
  MT8163 board support merged in, its `mt8163_arm32_defconfig` and a Pinna
  fragment. The userland is Debian trixie, whose kernel is 6.12 LTS, supported
  for trixie's lifetime.
- LibreEcho's delta to upstream 6.1.178 is 388 files: about 28.6k lines of
  board support (clocks, pinctrl, power domains, PMIC wrapper, MMC, SPI, MUSB,
  watchdog, thermal, LEDs, privacy switch, light sensors, codecs, AFE, FPGA SPI
  microphones, device tree) and the MT8163 CONSYS vendor driver (Wi-Fi/BT, 214k
  lines, a forward port of the 3.18 driver).
- The Dot has ~480 MB RAM, the kernel is monolithic (no modules), and the
  decompressed image must end below the ATF carve-out at 0x43000000.

## Decision

- **Source:** Debian's `linux` 6.12.117-1 — upstream stable v6.12.117 plus
  Debian's patch series from the kernel team's packaging tag, both pinned by
  commit in `kernel/source.env`.
- **Board support in `kernel/patches`**, applied after Debian's series: the
  LibreEcho board support ported to 6.12 (one patch per subsystem) with
  Pinna's audio and device-tree fixes, then the CONSYS driver (imported
  unmodified, then ported) with Pinna's Wi-Fi fixes. Not carried:
  diagnostic-only changes (SPI firmware-transfer tracing, FunctionFS and MUSB
  debug logging, a log line per watchdog ping), a `/proc` fix that is upstream
  in 6.12, and the Fire HD 8 (Giza) board with its bq24190 charger IDs (the
  Dot's charger node is disabled).
- **Device tree:** the boot image keeps using `libreecho-radar-puffin.dtb`
  from this kernel build (ADR 0001); the 6.12 build decompiles to the same
  tree as the DTB in the known-good LibreEcho `boot_a`.
- **The microphone FPGA bitstream is committed** (`kernel/firmware/`, 31 KB)
  and built in with `CONFIG_EXTRA_FIRMWARE`: the SPI microphone driver loads it
  at probe, before the root is mounted, so the device kernel cannot be built
  without it, and LibreEcho-Linux is its only public source. The Wi-Fi/BT
  firmware is committed for the same reason (`firmware/`, ADR 0001).
- **Config from allnoconfig + fragments.** `common.config` holds what every
  variant needs: the systemd requirements (README, v257), erofs/ext4/overlay,
  dm-verity, networking. `device.config`, `qemu.config` and `x86.config` add
  the hardware; `debug.config` (`DEBUG=1`, `just kernel-debug`) adds ftrace,
  debugfs, perf, sysrq and core dumps for dev images. Nothing else is enabled,
  and `kernel/build.sh` fails if a listed symbol does not land. The QEMU
  variants share the base, so the VM boot gates check that systemd comes up
  untainted on exactly this set.
- **Size:** no modules, XZ, `-Os`, `EXPERT` trimming, no tracing or debugfs in
  release kernels, only the drivers the Dot uses (Bluetooth keeps RFCOMM, not
  BNEP/HIDP; no USB host class drivers, ALSA sequencer or USB audio). Kept for
  robustness: `KALLSYMS` (readable oopses), pstore/ramoops (console and oops
  logs survive a warm reboot, in the DTB's `ramoops@44400000`) and the strong
  stack protector.

## Consequences

- Kernel updates move the pins to a newer Debian 6.12.y; `kernel/build.sh`
  fails if Debian's series or ours no longer applies.
- The ported clock, power-domain, USB, audio and Wi-Fi code is compile- and
  link-tested; only the device proves it (`just test-device`). Kaeru on the
  Echo Dot has no `fastboot boot`, so a new kernel goes straight to `boot_a`.
  A kernel that fails before userspace never arms the fastboot guard; fastboot
  (Volume Down + USB) is the way back to the previous boot image.
- `allnoconfig` drops every option a fragment does not list, including
  defaults the userland relies on (file locking, `MMC_BLOCK`, ALSA's verbose
  procfs). Before removing a line from a fragment, check who uses it; the VM
  gates cover only what QEMU exercises.
- A Thumb-2 kernel would shrink the image further; not done until the vendor
  driver has been checked on hardware with it.
