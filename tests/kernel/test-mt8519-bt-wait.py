#!/usr/bin/env python3
"""Only the MT7663 waits for its Wi-Fi before Bluetooth's setup (patch 0229).

btmtksdio's mt76xx_setup serves the MT7663 (Echo Dot (5th gen)) and the
MT7668 (the MT8516 Echos). The 3 s wait is for the MT7663's Wi-Fi firmware
load; the MT7668 must not pay it.
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
    source = (linux / "drivers/bluetooth/btmtksdio.c").read_text()
    setup = block(source, "static int mt76xx_setup(")
    lines = [l.strip() for l in setup.splitlines()]
    sleeps = [i for i, l in enumerate(lines) if re.match(r"msleep\(\s*\d+\s*\)", l)]
    if not sleeps:
        raise SystemExit("mt76xx_setup has no wait before its WMT commands (patch 0229)")
    for i in sleeps:
        cond = next(l for l in reversed(lines[:i]) if l)
        if not re.fullmatch(r"if \(bdev->data->chipid == 0x7663\)", cond):
            raise SystemExit(f"mt76xx_setup's {lines[i]} isn't limited to the MT7663 (under: {cond!r})")
    print("ok: only the MT7663 waits in mt76xx_setup")


if __name__ == "__main__":
    main()
