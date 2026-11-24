#!/usr/bin/env python3
"""Check that the MT8519's watchdog node has a driver match (patch 0222)."""
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
    source = (linux / "drivers/watchdog/mtk_wdt.c").read_text()
    soc = (linux / "arch/arm/boot/dts/mediatek/mt8519.dtsi").read_text()
    node = block(soc, "toprgu: toprgu@10007000")
    compatibles = re.findall(r'"([^"]+)"', re.search(r"compatible\s*=([^;]+);", node).group(1))
    table = block(source, "static const struct of_device_id mtk_wdt_dt_ids[]")
    matched = [
        (c, m.group(1))
        for c in compatibles
        if (m := re.search(r'\.compatible\s*=\s*"' + re.escape(c) + r'"\s*(?:,\s*\.data\s*=\s*&(\w+))?', table))
    ]
    if not matched:
        raise SystemExit(f"No mtk_wdt match for the MT8519's toprgu ({', '.join(compatibles)})")
    compatible, data = matched[0]
    if compatible != "mediatek,mt8519-rgu" or data != "mt8519_data":
        raise SystemExit(f"The toprgu matched {compatible} with {data}, not mediatek,mt8519-rgu with mt8519_data")
    count = re.search(r"#define\s+MT8519_TOPRGU_SW_RST_NUM\s+(\d+)", source)
    if not count or int(count.group(1)) != 18:
        raise SystemExit("MT8519_TOPRGU_SW_RST_NUM must be Amazon's 18 software resets")
    if "MT8519_TOPRGU_SW_RST_NUM" not in block(source, "static const struct mtk_wdt_data mt8519_data"):
        raise SystemExit("mt8519_data must carry the MT8519's reset count")
    print("ok: mtk_wdt binds the MT8519's toprgu with its 18 software resets")


if __name__ == "__main__":
    main()
