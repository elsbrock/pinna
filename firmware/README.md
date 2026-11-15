# MT8163 Wi-Fi and Bluetooth firmware

Installed into the device's root image as `/usr/lib/firmware/` by
`scripts/assemble-rootfs-extra.sh`. The CONSYS driver and the WMT helpers load
them when `pinna-wifi.service` powers on Wi-Fi and Bluetooth.

They are Amazon's files, unmodified, from the system partition of the Echo Dot
(2nd gen)'s stock Fire OS. They are not in linux-firmware or any other public
source, so they are kept here rather than left to each owner's backup.

| File | sha256 |
| --- | --- |
| `WIFI_RAM_CODE_8163` | `9669cc9b03cfdc5e8fd4fd6e14c4c4050e8c196738ca4707eea12f14a6a8e64c` |
| `ROMv2_lm_patch_1_0_hdr.bin` | `6642cfd4fd67accf27b0f9615e4cd0941417ddbc3b5bef1004fe1eb59235146f` |
| `ROMv2_lm_patch_1_1_hdr.bin` | `c4aa6dfc54b82f496d97be8eb3979232730f51a3fdfad34192b91d0b87b06a56` |
| `WMT_SOC.cfg` | `302bd4462de99c028c04092e561c1500d65582ce42a93c4c72ccae6e2c99013d` |
