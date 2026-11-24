#!/usr/bin/env python3
"""Check Cannoli codec DAI bindings and the TI ADC BCLK match."""
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

    helper = function_body(machine, "mt8519_mendocino_is_ti_adc")
    callsite = re.search(
        r"if\s*\((mt8519_mendocino_is_ti_adc\s*\(\s*"
        r"snd_soc_rtd_to_codec\s*\(\s*rtd\s*,\s*0\s*\)\s*\))\s*\)",
        machine,
    )
    if not callsite:
        raise SystemExit("BCLK inversion call site does not use the TI ADC helper")
    harness = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
struct device_node { const char *compatible; };
struct device { struct device_node *of_node; };
struct snd_soc_component { const char *name; struct device *dev; };
struct snd_soc_dai { const char *name; struct device *dev; struct snd_soc_component *component; };
struct snd_soc_pcm_runtime { struct snd_soc_dai *dais[1]; };
#define snd_soc_rtd_to_codec(rtd, n) ((rtd)->dais[n])
static bool of_device_is_compatible(const struct device_node *node, const char *compatible)
{
    return node && node->compatible && strcmp(node->compatible, compatible) == 0;
}
'''
    harness += helper + "\nstatic bool bclk_inversion_condition(struct snd_soc_pcm_runtime *rtd)\n{\n    return " + callsite.group(1) + ";\n}\n" + r'''
int main(void)
{
    struct device_node ti5140 = { "ti,tlv320adc5140" };
    struct device_node ti5120 = { "ti,tlv320adc5120" };
    struct device_node akm = { "asahi-kasei,ak5704" };
    struct device dev5140 = { &ti5140 }, dev5120 = { &ti5120 }, dev_akm = { &akm };
    struct device missing_node = { NULL };
    struct snd_soc_component ti_component = { "tlv320adcx140-codec", &dev5140 };
    struct snd_soc_component ti5120_component = { "tlv320adcx140-codec", &dev5120 };
    struct snd_soc_component akm_component = { "ak5704-codec", &dev_akm };
    struct snd_soc_component missing_component = { "tlv320adcx140-codec", &missing_node };
    struct snd_soc_dai generic_ti = { "tlv320adcx140-codec", &dev5140, &ti_component };
    struct snd_soc_dai generic_5120 = { "tlv320adcx140-codec", &dev5120, &ti5120_component };
    struct snd_soc_dai generic_akm = { "ak5704-hifi", &dev_akm, &akm_component };
    struct snd_soc_dai legacy5140 = { "tlv320adc5140-codec-a", NULL, NULL };
    struct snd_soc_dai legacy5120 = { "tlv320adc5120-codec-a", NULL, NULL };
    struct snd_soc_dai no_of = { "tlv320adcx140-codec", &missing_node, &missing_component };
    struct snd_soc_pcm_runtime rtd = { .dais = { &generic_ti } };
    assert(bclk_inversion_condition(&rtd));
    rtd.dais[0] = &generic_5120;
    assert(bclk_inversion_condition(&rtd));
    rtd.dais[0] = &generic_akm;
    assert(!bclk_inversion_condition(&rtd));
    rtd.dais[0] = &legacy5140;
    assert(bclk_inversion_condition(&rtd));
    rtd.dais[0] = &legacy5120;
    assert(bclk_inversion_condition(&rtd));
    rtd.dais[0] = &no_of;
    assert(!bclk_inversion_condition(&rtd));
    rtd.dais[0] = NULL;
    assert(!bclk_inversion_condition(&rtd));
    puts("MT8519 codec DAI registration and TI ADC BCLK match: PASS");
    return 0;
}
'''
    with tempfile.TemporaryDirectory(prefix="pinna-mt8519-audio-") as tmp:
        source = Path(tmp) / "audio-test.c"
        executable = Path(tmp) / "audio-test"
        source.write_text(harness)
        subprocess.run(
            [os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Wextra", "-Werror",
             str(source), "-o", str(executable)],
            check=True,
        )
        subprocess.run([str(executable)], check=True)


if __name__ == "__main__":
    main()
