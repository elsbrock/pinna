#!/usr/bin/env python3
"""Check that the MT8519's audio MCLK dividers round to the closest rate (patch 0235).

Amazon's clk-mtk.c hands a composite clock's flags to its divider; 6.12's
hands them to the clock core. The MT8519's three audio MCLK dividers need
CLK_DIVIDER_ROUND_CLOSEST: APLL2 runs at 196608001 Hz, and rounding down
took a divider of 9 for 24.576 MHz, 21.845 MHz on a C2N6L4.
"""
from pathlib import Path
import re
import sys


def main():
    linux = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    mediatek = linux / "drivers/clk/mediatek"
    header = (mediatek / "clk-mtk.h").read_text()
    common = (mediatek / "clk-mtk.c").read_text()
    soc = (mediatek / "clk-mt8519.c").read_text()

    composite = re.search(r"struct mtk_composite \{.*?\n\};", header, re.S).group(0)
    if "divider_flags" not in composite:
        raise SystemExit("struct mtk_composite has no divider_flags")
    register = common[common.index("mtk_clk_register_composite("):]
    if "div->flags = mc->divider_flags;" not in register[:register.index("\n}\n")]:
        raise SystemExit("mtk_clk_register_composite doesn't give the divider its flags")

    macro = re.search(r"#define DIV_GATE_FLAGS\(.*?\n\t\}", soc, re.S).group(0)
    if ".divider_flags = _flags" not in macro or re.search(r"\.flags = _flags", macro):
        raise SystemExit("The MT8519's DIV_GATE_FLAGS puts its flags on the clock, not the divider")
    for name in ("i2si1_mck", "tdmin_mck", "i2so1_mck"):
        entry = re.search(r'DIV_GATE_FLAGS\([^;]*?"' + name + r'"[^)]*\)', soc, re.S)
        if not entry or "CLK_DIVIDER_ROUND_CLOSEST" not in entry.group(0):
            raise SystemExit(f"{name} doesn't round to the closest rate")

    # What the flag changes: 24.576 MHz from APLL2's 196608001 Hz.
    parent, want = 196_608_001, 24_576_000
    assert -(-parent // want) == 9 and round(parent / want) == 8
    print("ok: the audio MCLK dividers round to the closest rate (24.576 MHz, not 21.845)")


if __name__ == "__main__":
    main()
