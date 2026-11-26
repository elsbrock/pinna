#!/usr/bin/env python3
"""Check Cannoli codec DAI bindings, and that every codec gets the link's own format."""
from pathlib import Path
import os
import re
import subprocess
import sys
import tempfile


def function_body(source, name, returns="bool "):
    start = source.index(f"static {returns}{name}(")
    brace = source.index("{", start)
    depth = 0
    for end in range(brace, len(source)):
        if source[end] == "{":
            depth += 1
        elif source[end] == "}":
            depth -= 1
            if depth == 0:
                return source[start:end + 1]
    raise ValueError(f"unterminated function: {name}")


def main():
    linux = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    dts = (linux / "arch/arm/boot/dts/mediatek/cannoli-cheesecake.dtsi").read_text()
    dai_sources = [
        linux / "sound/soc/codecs/max98396.c",
        linux / "sound/soc/codecs/tlv320adcx140.c",
    ]

    requested = re.findall(
        r"mediatek,[\w-]*codec-dai-name\s*=\s*\"([^\"]+)\";", dts
    )
    if not requested:
        raise SystemExit("No codec DAI names found in Cannoli device tree")

    registered = set()
    for path in dai_sources:
        source = path.read_text()
        arrays = re.findall(
            r"static\s+struct\s+snd_soc_dai_driver\s+\w+\[\]\s*=\s*\{(.*?)\n\};",
            source,
            re.S,
        )
        for array in arrays:
            registered.update(re.findall(r"\.name\s*=\s*\"([^\"]+)\"", array))
    unmatched = sorted(set(requested) - registered)
    if unmatched:
        raise SystemExit(
            f"Device-tree codec DAI names are not registered by the codec drivers: {unmatched}; "
            f"registered names: {sorted(registered)}"
        )

    machine = (linux / "sound/soc/mediatek/mt8519/mendocino.c").read_text()
    # Amazon's U-Boot writes its 5.4 names into /sound on every boot (seen on a
    # C2N6L4's UART, 2026-11-24): the machine driver must map them to names
    # the 6.12 codec drivers register, or the card defers forever.
    uboot_names = ["max98396-aif1-a", "tlv320adc5140-codec-a"]
    if machine.count("mt8519_mendocino_codec_dai_name(") < 3:
        raise SystemExit("Codec DAI names parsed from the device tree are not mapped")
    renamer = function_body(machine, "mt8519_mendocino_codec_dai_name", "const char *")
    rename_harness = "#include <assert.h>\n#include <stdio.h>\n#include <string.h>\n"
    rename_harness += "#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))\n" + renamer
    rename_harness += "\nint main(void)\n{\n    assert(mt8519_mendocino_codec_dai_name(NULL) == NULL);\n"
    rename_harness += '    assert(!strcmp(mt8519_mendocino_codec_dai_name("other"), "other"));\n'
    for name in uboot_names + requested:
        rename_harness += f'    puts(mt8519_mendocino_codec_dai_name("{name}"));\n'
    rename_harness += "    return 0;\n}\n"
    with tempfile.TemporaryDirectory(prefix="pinna-mt8519-rename-") as tmp:
        source = Path(tmp) / "rename.c"
        executable = Path(tmp) / "rename"
        source.write_text(rename_harness)
        subprocess.run([os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Wextra", "-Werror",
                        str(source), "-o", str(executable)], check=True)
        mapped = subprocess.run([str(executable)], check=True, capture_output=True,
                                text=True).stdout.split()
    unmatched = sorted(set(mapped) - registered)
    if unmatched:
        raise SystemExit(f"U-Boot's codec DAI names map to unregistered names: {unmatched}")

    # The TI converter takes the link's own BCLK polarity (patch 0233):
    # mainline's tlv320adcx140 inverts BCLK for DSP_A/B itself, where Amazon's
    # machine driver flipped it for its ADC driver.
    if "mt8519_mendocino_is_ti_adc" in machine:
        raise SystemExit("The machine driver still flips BCLK for the TI converter")
    print("MT8519 codec DAI registration and link formats: PASS")

if __name__ == "__main__":
    main()
