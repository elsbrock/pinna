#!/usr/bin/env python3
"""The Echo Dot (5th gen)'s MT7663 uses one spatial stream on both paths (patch 0237).

Patch 0228 enables both TX paths for the 0x7653 efuse; the antenna is on WF1.
mt76 builds each band's HT and VHT streams from antenna_mask while it
registers them, so antenna_mask is one chain during mt76_register_device and
both chains again right after: mt7615 also takes the received signal
(status->chains) and the TX power per chain from it. The channel command asks
the firmware for one TX stream. With two streams the unit retried about every
frame it sent (C2N6L4, 2026-11-28).
"""
from pathlib import Path
import re
import sys


def block(text, anchor):
    start = text.index(anchor)
    brace = text.index("{", start)
    depth = 1
    end = brace + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start:end]


def main():
    linux = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    mt7615 = linux / "drivers/net/wireless/mediatek/mt76/mt7615"
    eeprom = (mt7615 / "eeprom.c").read_text()
    init = (mt7615 / "init.c").read_text()
    mcu = (mt7615 / "mcu.c").read_text()
    usb_sdio = (mt7615 / "usb_sdio.c").read_text()

    if "dev->one_stream = true;" not in block(eeprom, "static void mt7663_eeprom_fixup("):
        raise SystemExit("mt7663_eeprom_fixup doesn't mark the 0x7653 efuse for one stream (patch 0237)")

    reg = block(usb_sdio, "int mt7663_usb_sdio_register_device(")
    one = re.search(r"if \(dev->one_stream\)\s*dev->mphy\.antenna_mask = BIT\(0\);", reg)
    call = reg.find("mt76_register_device(")
    back = reg.find("dev->mphy.antenna_mask = dev->chainmask;", call)
    if not one or one.start() > call:
        raise SystemExit("the bands are registered with two streams: no one-chain antenna_mask before mt76_register_device")
    if back < 0 or back > reg.find("if (err < 0)", call):
        raise SystemExit("antenna_mask isn't both chains again right after mt76_register_device")

    chan = block(mcu, "int mt7615_mcu_set_chan_info(")
    if not re.search(r"\.tx_streams = dev->one_stream \? 1 :", chan):
        raise SystemExit("the channel command still asks for a TX stream per path")

    # The signal and the TX power per chain read antenna_mask: it stays both chains.
    for name, text, anchor in (
        ("mt7615_eeprom_parse_hw_cap", eeprom, "static void mt7615_eeprom_parse_hw_cap("),
        ("mt7615_cap_dbdc_disable", init, "mt7615_cap_dbdc_disable(struct mt7615_dev *dev)"),
    ):
        if "dev->mphy.antenna_mask = dev->chainmask;" not in block(text, anchor):
            raise SystemExit(f"{name} no longer sets antenna_mask to both chains")
    print("ok: the 0x7653 MT7663 registers one stream and keeps both chains")


if __name__ == "__main__":
    main()
