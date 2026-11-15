# First flash: Echo Dot (2nd gen) → Pinna

Moves an Echo Dot that currently runs LibreEcho (Kaeru/amonet-unlocked) to
Pinna. Every step says what it writes and how to undo it.

## What gets written

| Partition | Before | After |
|---|---|---|
| `boot_a` (p10) | LibreEcho boot image | Pinna boot image (`build/pinna-boot.img`) — the one LK always loads |
| `boot_b` (p11) | LibreEcho copy | Pinna boot image (manual fallback copy) |
| `system_a` (p13) | stock Fire OS system (LibreEcho reads Wi-Fi firmware from it) | Pinna erofs root (`mkosi/mkosi.output/pinna.root-arm.raw`) |
| `userdata` (p16) | LibreEcho `/data` | wiped; Pinna formats it as `/var` on first boot |

Never written: preloader/boot areas, `lk_a`/`lk_b`, `tee1`/`tee2`, `expdb`
(Kaeru payload), `persist` (identity, calibration, MAC), `misc` (only the
bootloader command field, at runtime, by the fastboot guard).

The Wi-Fi firmware that used to come from `system_a` is baked into the Pinna
root (`blobs/firmware/`, extracted from the backup).

## 0. Build

```sh
nix develop
just kernel models image bootimg
scripts/flash-dot.sh check      # needs the device in fastboot, see step 2
```

## 1. Backup (done 2026-11-15)

`~/echo-backups/echodot2-2026-11-15/`: full `mmcblk0.img`, `mmcblk0boot{0,1}.img`,
and per-partition images under `partitions/` with `SHA256SUMS`. `boot_a`/`boot_b`
hash `99f17f9e…` = the LibreEcho image that was running.

Optional: verify against the live device before changing anything (LibreEcho, adb):

```sh
for p in 10 11 13 16; do adb shell sha256sum /dev/mmcblk0p$p; done
```

## 2. Enter fastboot

Unplug, hold **Volume Down**, plug the micro-USB cable in, release when the
ring changes. (On Kaeru units the Action button does nothing.)

```sh
fastboot devices
scripts/flash-dot.sh check     # product must be BISCUIT
```

## 3. Flash the root filesystem

```sh
scripts/flash-dot.sh root      # system_a <- erofs root, userdata wiped
```

If LK rejects the 150 MB raw image (max-download-size), convert first:
`img2simg pinna.root-arm.raw pinna.root.simg` and flash that.

## 4. Try the kernel from RAM first

```sh
scripts/flash-dot.sh try       # fastboot boot build/pinna-boot.img
```

Nothing is written. If Kaeru does not support `fastboot boot`, skip to step 5;
the recovery path is the same.

Within ~20 s the host should see a new USB device "Pinna (Echo Dot 2nd gen)":

- serial console: `/dev/ttyACM0` (root autologin) — `picocom -b 115200 /dev/ttyACM0`
- network: the host gets 10.42.0.x by DHCP; `ssh root@10.42.0.1` (or `root@pinna.local`)

Then check:

```sh
systemctl --failed
journalctl -b -u pinna-wifi -u wpa_supplicant@wlan0 -u pinnad
ip addr show wlan0
pinna-bootctl status
```

## 5. Make it persistent

From Pinna: `pinna-bootctl reboot-bootloader` (or Volume Down + replug), then

```sh
scripts/flash-dot.sh boot      # boot_a + boot_b <- Pinna, reboot
```

## 6. Home Assistant

Pinna advertises `_esphomelib._tcp` via mDNS. Home Assistant should show
"Echo Dot" as a discovered ESPHome device (plaintext API, no key yet). Add it,
then configure the voice assistant pipeline for the device. Say "Alexa…".

Things expected to need tuning on real hardware (not testable off-device):
mic gain and wake threshold (`/etc/pinna/pinnad.toml`, `journalctl -u pinnad`
shows scores), speaker mixer sequence, LED direction.

## Recovery

Any time, from fastboot (Volume Down + plug in):

```sh
scripts/flash-dot.sh restore ~/echo-backups/echodot2-2026-11-15/partitions
```

restores LibreEcho exactly (boot_a, boot_b, system_a, userdata).

### Fastboot guard

`/etc/pinna/fastboot-guard` (present in dev images) makes every boot arm a
one-shot "enter fastboot" request early, cleared once sshd and pinnad are up
(`pinna-boot-ok.service`). A boot that hangs, followed by a power cycle, lands
in fastboot instead of looping. Kaeru clears the request itself, so
`fastboot reboot` tries Pinna again.
