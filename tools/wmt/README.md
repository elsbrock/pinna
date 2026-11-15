# MT8163 WMT helpers

Userspace helpers that power up the MT8163 combo chip (Wi-Fi/Bluetooth) through
the vendor WMT driver: `wmt_stock_compat` (configure-only init), `wmt_launcher`
(one-shot command responder), `wmt_configure` and `wmt_bt_on` (Bluetooth).

Copied unchanged from LibreEcho-Platform `tools/mt8163-arm32/connectivity/`
(GPL-2.0, see the SPDX headers); upstream revision in `.upstream-rev`.
Built as static musl binaries by `just wmt`.
