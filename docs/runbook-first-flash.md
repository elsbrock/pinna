# First flash: Echo Dot (2nd gen) → Pinna

Moves an Echo Dot that currently runs LibreEcho (Kaeru/amonet-unlocked) to
Pinna. Every step says what it writes and how to undo it.

## What gets written

| Partition | Before | After |
|---|---|---|
| `boot_a` (p10) | LibreEcho boot image | Pinna boot image (`build/pinna-boot.img`: kernel, DTB, initramfs with pinna-init) — the one LK always loads |
| `boot_b` (p11) | LibreEcho copy | Pinna boot image (manual fallback copy) |
| `system_a` (p13) | stock Fire OS system (LibreEcho reads Wi-Fi firmware from it) | Pinna root, sealed slot image (`build/pinna.root.img`: erofs + signed footer + dm-verity hash tree) |
| `system_b` | stock copy | first MiB zeroed (no image); later written by `pinnactl update` |
| `userdata` (p16) | LibreEcho `/data` | wiped; Pinna formats it as `/var` on first boot |

Never written: preloader/boot areas, `lk_a`/`lk_b`, `tee1`/`tee2`, `expdb`
(Kaeru payload), `persist` (identity, calibration, MAC). `misc` only at
runtime: the bootloader command field (fastboot guard) and Pinna's A/B slot
state at offset 0x40000 (ADR 0003).

The Wi-Fi firmware that used to come from `system_a` is baked into the Pinna
root (`blobs/firmware/`, extracted from the backup).

## 0. Build

```sh
nix develop
just keygen                     # once: secrets/release.key signs root images
just kernel models image bootimg
scripts/flash-dot.sh check      # needs the device in fastboot, see step 2
```

`just image` builds the erofs root and seals it into `build/pinna.root.img`;
only images signed with the key in `keys/release.pub` (compiled into the boot
image's pinna-init) boot. After changing the key, flash the new boot image and
a root signed with the new key together (steps 3 and 5): `pinnactl update`
cannot cross a key change.

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
scripts/flash-dot.sh root      # system_a <- sealed root, system_b cleared, userdata wiped
```

If LK rejects the ~160 MB image (max-download-size), convert first:
`img2simg build/pinna.root.img build/pinna.root.simg` and flash that with
`ROOT_IMG=build/pinna.root.simg scripts/flash-dot.sh root`. The script checks
the signature of raw images only, so verify the raw one first:
`cargo run -q --release -p pinna-image -- verify build/pinna.root.img`.

## 4. Try the kernel from RAM first

```sh
scripts/flash-dot.sh try       # fastboot boot build/pinna-boot.img
```

Nothing is written. If Kaeru does not support `fastboot boot`, skip to step 5;
the recovery path is the same.

On the UART console (if attached) pinna-init reports first:
`pinna-init: no slot state (first flash): slot a`, then
`pinna-init: slot mmcblk0p13 -> dm-0 (dm-verity, root hash …)` and
`pinna-init: booting slot a, version …`.

Within ~20 s the host should see a new USB device "Pinna (Echo Dot 2nd gen)":

- serial console: `/dev/ttyACM0` (root autologin) — `picocom -b 115200 /dev/ttyACM0`
- network: the host gets 10.42.0.x by DHCP; `ssh root@10.42.0.1` (or `root@pinna.local`)

Then check:

```sh
pinnactl status
systemctl --failed
journalctl -b -u pinna-wifi -u wpa_supplicant@wlan0 -u pinnad
ip addr show wlan0
pinna-bootctl status
pinna-bootctl slot-status    # active a, a good (after pinna-boot-ok), booted a
findmnt /                    # /dev/dm-0 erofs (dm-verity)
pinnactl led-test            # ring animation
pinnactl volume 40
```

## 5. Make it persistent

From Pinna: `pinna-bootctl reboot-bootloader` (or Volume Down + replug), then

```sh
scripts/flash-dot.sh boot      # boot_a + boot_b <- Pinna, reboot
```

## 6. Home Assistant

Pinna advertises `_esphomelib._tcp` via mDNS. Home Assistant should show
"Echo Dot" as a discovered ESPHome device. Add it, then configure the voice
assistant pipeline for the device. Say "Alexa…".

The API is plaintext until a key is set: put a base64 32-byte key in
`[esphome] encryption_key` (`/etc/pinna/pinnad.toml`, i.e. in the image) or in
`/var/lib/pinna/esphome-key`, and give Home Assistant the same key. Home
Assistant can then rotate it over the encrypted connection; the new key is
stored in `/var/lib/pinna/esphome-key`.

Things expected to need tuning on real hardware (not testable off-device):
mic gain and wake threshold (`/etc/pinna/pinnad.toml`, `journalctl -u pinnad`
shows scores), speaker mixer sequence, LED direction.

## 7. Updates and rollback

On the build machine:

```sh
just release                   # build/release/{pinna-<version>.root.img, pinna-boot.img, manifest.json}
cd build/release && python3 -m http.server 8000
```

On the device (as root):

```sh
pinnactl update http://<host>:8000/manifest.json --reboot
```

This writes the image into the slot that is not booted (refusing while the
booted slot is still on trial), checks size and sha256 while writing, reads
it back (signature, whole dm-verity hash tree) and only then makes it active
for a trial boot with 3 tries (`--tries N`). After the reboot pinna-init boots
the new slot; once `pinna-boot-ok.service` passes (pinnad answers, sshd up),
the slot is marked good. If the health check fails or does not pass within
5 minutes, the device reboots; each boot uses up a try, and with none left
pinna-init goes back to the previous slot. `pinna-bootctl slot-status` shows
the state and the image version in each slot; `pinna-bootctl set-active a`
switches by hand (as a trial).

`boot_a` (kernel, initramfs) is not A/B: a new kernel or pinna-init is
flashed with `scripts/flash-dot.sh boot` as before.

## Recovery

Any time, from fastboot (Volume Down + plug in):

```sh
scripts/flash-dot.sh restore ~/echo-backups/echodot2-2026-11-15/partitions
```

restores LibreEcho exactly (boot_a, boot_b, system_a, userdata).

Restoring LibreEcho also restores `system_b` when the backup has it.

### Fastboot guard

`/etc/pinna/fastboot-guard` (present in dev images) makes a boot arm a
one-shot "enter fastboot" request early, cleared once sshd and pinnad are up
(`pinna-boot-ok.service`). A boot that hangs, followed by a power cycle (or a
failed health check, which reboots), lands in fastboot instead of looping.
Kaeru clears the request itself, so `fastboot reboot` tries Pinna again.

It is armed only when the booted slot is already good or there is no good
fallback. During the trial boot of a freshly installed slot with a good
previous slot it is not armed (`not arming: trial boot of slot b …`): a failed
trial reboots and rolls back to the previous slot instead of stopping in
fastboot.
