# First flash: Echo Dot (2nd gen) → Pinna

Moves an Echo Dot that currently runs the previous firmware (Kaeru/amonet-unlocked) to
Pinna. Every step says what it writes; Recovery (at the end) undoes all of
them from the step 1 backup.

## What gets written

| Partition | Before | After |
|---|---|---|
| `boot_a` (p10) | previous firmware's boot image | Pinna boot image (`build/pinna-boot.img`: kernel, DTB, initramfs with pinna-init), marked `pinna.boot=a`: slot A's kernel |
| `boot_b` (p11) | previous firmware's copy | the same image marked `pinna.boot=b`: slot B's kernel (ADR 0019) |
| `system_a` (p13) | stock Fire OS system (the previous firmware reads Wi-Fi firmware from it) | Pinna root, sealed slot image (`build/pinna.root.img`: erofs + signed footer + dm-verity hash tree) |
| `system_b` | stock copy | first MiB zeroed (no image); later written by `pinnactl update` |
| `userdata` (p16) | previous firmware's `/data` | wiped; Pinna formats it as `/var` on first boot |

Never written: preloader/boot areas, `lk_a`/`lk_b`, `tee1`/`tee2`, `expdb`
(Kaeru payload), `persist` (identity, calibration, MAC). `misc`: `root` and
`restore` set Kaeru's slot record (0x360) to slot a with `fastboot set_active
a`; at runtime Pinna writes the bootloader command field (fastboot guard) and
that record (ADR 0019).

The Wi-Fi firmware that used to come from `system_a` is baked into the Pinna
root (`firmware/` in the repository). The microphone FPGA bitstream is in the
repository too (`kernel/firmware/`) and built into the kernel.

## 0. Tools

Two ways in: from a release (no build), or from a build of the repository.

**From a release**, you need `fastboot`, `adb` and `sha256sum` from your
distribution (on Debian/Ubuntu, `android-tools` has the first two), and all
five files of one [release](https://github.com/elsbrock/pinna/releases)
downloaded into a single folder. The commands below run in that folder; where
they say `scripts/flash-gen2.sh`, use `./flash-gen2.sh` there.

**From a build**:

```sh
git clone <this repository> pinna && cd pinna
nix develop          # recommended dev shell (x86_64 Linux); needs flakes:
                     # nix --extra-experimental-features 'nix-command flakes' develop
                     # optional: without Nix, install the tools flake.nix lists
```

The commands below run in this shell (or with the tools you installed), from
the repository root.

Either way, `adb devices` must list the Echo Dot (the previous firmware
running, USB connected).

## 1. Back up, and save what Home Assistant knows (the previous firmware still running)

Step 4 overwrites `system_a` and wipes `userdata`, and recovery restores from
this backup. Do not continue without it.

```sh
scripts/backup-gen2.sh ~/echo-backups/echodot2-$(date +%F)
```

It needs about 8 GB free. It pulls the whole eMMC and its boot areas over
adb (~4 GB), compares every
partition except `userdata` with the device, and cuts `partitions/*.img` with
`SHA256SUMS`, the layout `scripts/flash-gen2.sh restore` takes. Copy the
directory somewhere safe. `scripts/verify-backup.sh <dir>` repeats the
comparison later, as long as the previous firmware still runs.

**Taking over the previous firmware's device in Home Assistant** (keeps its entities,
automations and scripts; see step 7): `userdata` holds the previous firmware's API key and
node name, so save them now:

```sh
mkdir -p secrets/device
adb shell 'cat /data/*/config/web-config.json' | tr -d '\r' > secrets/previous-web-config.json
python3 - secrets/previous-web-config.json <<'EOF'
import json, sys
c = json.load(open(sys.argv[1]))
open("secrets/device/esphome-key", "w").write(c["esphome_noise_key"] + "\n")
if "hostname" not in c:
    sys.exit("no hostname in web-config.json: write [device] name = \"<node name>\" "
             "to secrets/device/pinnad.toml, using the node name Home Assistant shows")
open("secrets/device/pinnad.toml", "w").write('[device]\nname = "%s"\n' % c["hostname"])
EOF
cat secrets/device/pinnad.toml
```

`hostname` is the previous firmware's node name. If it was never set, the
file has none and the snippet stops: take the node name from the device's
entry in Home Assistant (ESPHome integration) instead.
`secrets/device/pinnad.toml` must be valid TOML: pinnad refuses to start on
an error or an unknown key, and on a first flash that ends in fastboot (see
Recovery).

## 2a. Download a release

A [release](https://github.com/elsbrock/pinna/releases) carries five files:
`pinna-<version>.root.img`, `pinna-boot.img`, `manifest.json`, `SHA256SUMS`
and `flash-gen2.sh`. Download all of them into one folder and check them
against the checksums:

```sh
sha256sum -c SHA256SUMS
```

## 2b. Or build it yourself

`secrets/` (gitignored) holds what makes the image yours:

| File | Needed | Ends up as | Create with |
|---|---|---|---|
| `secrets/release.key` | only to sign your own builds | root image signing key | `just keygen` (writes `secrets/release.key` and `secrets/release.pub`; the build compiles in `secrets/release.pub` instead of `keys/release.pub`) |
| `secrets/machine-id` | no (each device derives its machine id from its serial number) | — | — |
| `secrets/authorized_keys` | for SSH | `/root/.ssh/authorized_keys` | your SSH public key(s) |
| `secrets/wifi/wpa_supplicant-wlan0.conf` | for Wi-Fi | default for `/var/lib/pinna/wifi/` (first boot; edit on the device afterwards) | see below |
| `secrets/device/pinnad.toml` | no | default for `/var/lib/pinna/pinnad.toml` — device identity: a `[device]` table with e.g. `name = "echodot2"` on its own line | step 1, or by hand |
| `secrets/device/esphome-key` | no | default for `/var/lib/pinna/esphome-key` — the Home Assistant API key (base64, 32 bytes) | `head -c32 /dev/urandom \| base64`, or the previous firmware's (step 1) |

Without an API key, Home Assistant adds the device without asking for one and
then gives it a key of its own (ADR 0018). Without Wi-Fi credentials the device is
reachable over USB only. A minimal Wi-Fi file:

```
ctrl_interface=DIR=/run/wpa_supplicant GROUP=netdev
country=DE          # your country code

network={
    ssid="your-network"
    psk="your-passphrase"
    scan_ssid=1
}
```

The `/var` defaults are copied only when missing, so a re-flash (which wipes
`/var`) comes back as the same Home Assistant device on the same network.

```sh
mkdir -p secrets/device secrets/wifi
cat ~/.ssh/id_*.pub > secrets/authorized_keys
just keygen                     # once: secrets/release.key signs your own images
just all                        # kernel, wake word models, root image, boot image
```

`just image` builds the erofs root and seals it into `build/pinna.root.img`.
A device that runs a release only boots roots signed with the project key in
`keys/release.pub` (compiled into the boot image's pinna-init), so flashing
your own build means flashing the root and the boot image together (steps 4
and 5): `pinnactl update` cannot cross a key change.

## 3. Enter fastboot

Unplug, hold **Volume Down**, plug the micro-USB cable in, release when the
ring changes. (On Kaeru units the Action button does nothing.)

```sh
fastboot devices
scripts/flash-gen2.sh check     # product must be BISCUIT (Dot) or RADAR (Echo); lists the images
```

## 4. Flash the root filesystem

In the release folder you downloaded (step 2a):

```sh
./flash-gen2.sh root            # system_a <- the release root, system_b cleared, userdata wiped
```

In the repository (step 2b):

```sh
scripts/flash-gen2.sh root      # system_a <- sealed root, system_b cleared, userdata wiped
```

The script checks the files against `SHA256SUMS` when they come from a
release, and the image's signature when you built it yourself. LK accepts
109 MiB per download (`max-download-size` 0x6d00000); fastboot splits the
larger root image into sparse chunks by itself.

Undo: Recovery. From here until step 5, the previous firmware no longer boots properly.

## 5. Flash the boot image

```sh
./flash-gen2.sh boot            # in the release folder: boot_a + boot_b <- Pinna, reboot
```

From the repository:

```sh
scripts/flash-gen2.sh boot      # boot_a + boot_b <- Pinna, reboot
```

Undo: Recovery.

## 6. First boot checks

In the kernel log (`dmesg | grep pinna-init` once you're logged in; the Echo
Dot has no UART) pinna-init reports first:
`pinna-init: slot a, as the bootloader started it (good attempts=0 (priority 15, tries 0))`, then
`pinna-init: slot mmcblk0p13 -> dm-0 (dm-verity, root hash …)` and
`pinna-init: booting slot a, version …`.

Within ~20 s the host should see a new USB device "Pinna (Echo Dot 2nd gen)":

- serial console: `/dev/ttyACM0` (root autologin) — `picocom -b 115200 /dev/ttyACM0`
  (picocom is not in the dev shell)
- network: the host gets 10.42.0.x by DHCP; `ssh root@10.42.0.1` (or `root@pinna-<mac suffix>.local`)

A device named "Pinna rescue" instead means pinna-init found nothing to boot
and stayed in rescue mode (ADR 0012): http://10.42.0.1 says why and offers the
logs as one download; `/dev/ttyACM0` has a root shell.

A release image carries no SSH key. Add yours over the serial console:

```sh
mkdir -p /var/lib/pinna/ssh && cat >> /var/lib/pinna/ssh/authorized_keys
# paste your public key, then press Ctrl-D
```

A factory reset removes it again.

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

If it does not come up, see Recovery below.

## 7. Home Assistant

Pinna advertises `_esphomelib._tcp` via mDNS.

- **New device**: Home Assistant shows "Echo Dot" as a discovered ESPHome
  device. Add it; if `secrets/device/esphome-key` was set, it asks for that
  key. Then configure the voice assistant pipeline for the device.
- **Taking over the previous firmware's entry** (step 1): do not add a new device. Pinna
  exposes the previous firmware's entity set under the same names, so the existing
  entities, automations and scripts keep working. Reload the existing ESPHome
  config entry once so Home Assistant rebuilds the platforms from Pinna's
  device info.

Say "Alexa…".

Without a key in the image, the API is plaintext until Home Assistant sets
one when you add the device (ADR 0018). To set one by hand instead, put a
base64 32-byte key in `/var/lib/pinna/esphome-key` (or `[esphome]
encryption_key` in `/var/lib/pinna/pinnad.toml`), restart pinnad, and give
Home Assistant the same key. Home Assistant can rotate the key over the
encrypted connection; the new key is stored in `/var/lib/pinna/esphome-key`.

Things expected to need tuning on real hardware (not testable off-device):
mic gain and wake threshold (`/var/lib/pinna/pinnad.toml` on the device; it
overrides the image's `/etc/pinna/pinnad.toml`; `systemctl restart pinnad`
after editing, `journalctl -u pinnad` shows scores), speaker mixer sequence,
LED direction.

## 8. Updates and rollback

Release images look for new releases on their update channel
(`updates.getpinna.dev`, ADR 0022) and show them on the dashboard's **System > Updates**
page and in Home Assistant, where you install them. From the device, the same channel by hand (it needs Wi-Fi with internet
access):

```sh
pinnactl update https://updates.getpinna.dev/v1/channels/stable --channel stable --reboot
```

For your own builds, on the build machine:

```sh
just release                   # build/release/{pinna-<version>.root.img, pinna-boot.img, manifest.json, manifest.json.sig}
cd build/release && python3 -m http.server 8000
```

If you used `just push`, first remove the dev overlay; it would otherwise
keep shadowing the new image's binaries:
`PINNA_HOST=root@10.42.0.1 scripts/push-dev.sh --remove`.

Then, from the build machine, tunnel the server over the device's SSH session
and update:

```sh
ssh -R 8000:localhost:8000 root@10.42.0.1 \
  pinnactl update http://127.0.0.1:8000/manifest.json --reboot
```

On the Echo Dot over USB, download, write and full verification of a 153 MiB
root take ~47 s. With the device on Wi-Fi, `pinnactl update
http://<build machine's LAN address>:8000/manifest.json --reboot` on the
device works too, if the build machine's firewall lets port 8000 in.

This writes the image into the slot that is not booted (refusing while the
booted slot is still on trial), checks size and sha256 while writing, reads
it back (signature, whole dm-verity hash tree) and only then makes it active
for a trial boot with 3 attempts (`--tries N`) in the bootloader's slot record
(ADR 0019). The update also writes the release's boot image into the target
slot's boot partition. After the reboot the bootloader starts the new slot,
kernel and root; once `pinna-boot-ok.service` passes (pinnad answers, sshd
up), the slot is marked good. If the health check fails or does not pass
within 5 minutes, the device reboots; the preloader counts each boot, and
with no attempts left the bootloader starts the previous slot again. `pinna-bootctl slot-status` shows
the state and the image version in each slot; `pinna-bootctl set-active a`
switches by hand (as a trial).

An update into slot A writes `boot_a` only once a boot from slot B has shown
that the bootloader loads `boot_b` (`/var/lib/pinna/kernel-slots`); a Kaeru
before `ab93750` (22 October 2026) always loads `boot_a`; once a boot from
slot b has shown that, updates write no kernel at all. There, flash new boot
images by hand:
`pinna-bootctl reboot-bootloader` on the device, then
`scripts/flash-gen2.sh boot` (step 5).
When the signing key changed — you built with a new `secrets/release.key` —
`pinnactl update` cannot help: flash root and boot together (steps 3–5).
`keys/release.pub` is the project key and doesn't change; your own key is in
`secrets/`.

## Recovery

Any time, from fastboot (Volume Down + plug in, or `pinna-bootctl
reboot-bootloader` on a running Pinna):

```sh
scripts/flash-gen2.sh restore <backup dir>/partitions
```

restores the previous firmware exactly from the step 1 backup (boot_a, boot_b, system_a,
userdata, and system_b when the backup has it). It checks the backup's
`SHA256SUMS` and that every required image is there before writing anything.

### Fastboot guard

`/etc/pinna/fastboot-guard` (in every image built from this tree; it only acts
when the Kaeru payload is present in `expdb`) makes a boot arm a
one-shot "enter fastboot" request early, cleared once sshd and pinnad are up
(`pinna-boot-ok.service`). A boot that hangs, followed by a power cycle (or a
failed health check, which reboots), lands in fastboot instead of looping.
Kaeru clears the request itself, so `fastboot reboot` tries Pinna again.

It is armed only when the booted slot is already good or there is no good
fallback. During the trial boot of a freshly installed slot with a good
previous slot it is not armed (`not arming: trial boot of slot b …`): a failed
trial reboots and rolls back to the previous slot instead of stopping in
fastboot.

## Development loop and hardware gate

```sh
PINNA_HOST=root@10.42.0.1 just push          # fresh binaries as a sysext overlay, pinnad restarted
PINNA_HOST=root@10.42.0.1 scripts/push-dev.sh --remove  # back to the image's binaries (before an update)
PINNA_HOST=root@10.42.0.1 PINNA_API_KEY=secrets/device/esphome-key \
  THROUGHPUT_URL=http://<lan-host>/<file> just test-device
```

`just test-device` runs the boot checks on the real device plus the Home
Assistant entity set, a Wi-Fi throughput floor and an acoustic test: with the
PipeWire microphone graph (ADR 0004) the device plays music and its wake word
through its own speaker, and the echo canceller must remove at least 20 dB of
it without the device waking on its own voice. The acoustic test needs a
quiet room; `PINNA_ACOUSTIC=0` skips it.

## Known limitations

- **Wi-Fi**: the vendor driver reaches 1–1.7 MB/s on 2.4 GHz. It needs
  Amazon's small 2.4 GHz block-ack window (kernel patch 0028): without it,
  transfers that start after a few idle seconds stall for 10–30 s.
- **Echo cancellation** needs a few seconds of playback to converge; the first
  seconds of new audio are only partly removed.

