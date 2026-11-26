#!/usr/bin/env python3
"""Check that a device-tree codec gets a link component of its own (patch 0231).

The sound card's eTDM links are declared with COMP_DUMMY() codecs, an empty
array on 6.12. This compiles the machine driver's codec parser against stubs
and runs it on a link whose codec array is empty, with a canary right after
it: the parser must give the link its own component and leave the canary
alone.
"""
from pathlib import Path
import os
import subprocess
import sys
import tempfile


def function_body(source, signature):
    start = source.index(signature)
    brace = source.index("{", start)
    depth = 0
    for end in range(brace, len(source)):
        if source[end] == "{":
            depth += 1
        elif source[end] == "}":
            depth -= 1
            if depth == 0:
                return source[start:end + 1]
    raise ValueError(f"unterminated function: {signature}")


HARNESS = r'''
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define PREFIX "mediatek,"
#define GFP_KERNEL 0

struct device { int unused; };
struct device_node { const char *name; };
struct snd_soc_dai_link_component {
    const char *name;
    struct device_node *of_node;
    const char *dai_name;
};
struct snd_soc_dai_link {
    struct snd_soc_dai_link_component *codecs;
    unsigned int num_codecs;
};

static struct device_node adc = { "tlv320adc5140@4c" };
static int puts_nodes;

static int of_property_read_u32(struct device_node *np, const char *prop, unsigned int *v)
{
    (void)np; (void)prop; (void)v;
    return -EINVAL;
}

static struct device_node *of_parse_phandle(struct device_node *np, const char *prop, int i)
{
    (void)np; (void)i;
    return strcmp(prop, "mediatek,etdm1-in-audio-codec") ? NULL : &adc;
}

static int of_property_read_string(struct device_node *np, const char *prop, const char **out)
{
    (void)np;
    if (strcmp(prop, "mediatek,etdm1-in-codec-dai-name"))
        return -EINVAL;
    *out = "tlv320adc5140-codec-a"; /* as Amazon's U-Boot writes it */
    return 0;
}

static void of_node_put(struct device_node *np) { if (np) puts_nodes++; }

static void *devm_kzalloc(struct device *dev, size_t size, int flags)
{
    (void)dev; (void)flags;
    return calloc(1, size);
}
'''

MAIN = r'''
/* COMP_DUMMY() on 6.12: a codec array of size 0, then whatever comes next. */
static struct {
    struct snd_soc_dai_link_component empty[0];
    struct snd_soc_dai_link_component canary;
} layout = { .canary = { "canary", NULL, "canary-dai" } };

static void check_canary(void)
{
    assert(!strcmp(layout.canary.name, "canary"));
    assert(layout.canary.of_node == NULL);
    assert(!strcmp(layout.canary.dai_name, "canary-dai"));
}

int main(void)
{
    struct device dev = { 0 };
    struct device_node np = { "sound" };
    struct snd_soc_dai_link adc_link = { layout.empty, 0 };
    struct snd_soc_dai_link plain_link = { layout.empty, 0 };

    mt8519_mendocino_parse_of_codec(&dev, &np, &adc_link, "etdm1-in");
    check_canary();
    assert(adc_link.num_codecs == 1);
    assert(adc_link.codecs != layout.empty);
    assert(adc_link.codecs->of_node == &adc);
    assert(adc_link.codecs->name == NULL);
    assert(!strcmp(adc_link.codecs->dai_name, "tlv320adcx140-codec"));

    /* No codec in the device tree: the link stays on the dummy. */
    mt8519_mendocino_parse_of_codec(&dev, &np, &plain_link, "etdm2-in");
    check_canary();
    assert(plain_link.num_codecs == 0);
    assert(plain_link.codecs == layout.empty);
    assert(puts_nodes == 0);
    puts("ok: a device-tree codec gets its own link component");
    return 0;
}
'''


def main():
    linux = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    machine = (linux / "sound/soc/mediatek/mt8519/mendocino.c").read_text()
    renamer = function_body(machine, "static const char *mt8519_mendocino_codec_dai_name(")
    parser = function_body(machine, "static void mt8519_mendocino_parse_of_codec(")
    with tempfile.TemporaryDirectory(prefix="pinna-mt8519-links-") as tmp:
        source = Path(tmp) / "links.c"
        executable = Path(tmp) / "links"
        source.write_text(HARNESS + renamer + "\n" + parser + "\n" + MAIN)
        subprocess.run([os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Wextra",
                        "-Wno-unused-variable", "-Wno-unused-but-set-variable",
                        "-Wno-unused-function",
                        "-fno-strict-aliasing", "-O0", str(source), "-o", str(executable)],
                       check=True)
        result = subprocess.run([str(executable)], capture_output=True, text=True)
        if result.returncode != 0:
            raise SystemExit(
                "The codec parser wrote past a COMP_DUMMY() link's codec array or left the "
                f"device-tree codec off its link (exit {result.returncode}): "
                f"{result.stderr.strip()}"
            )
        print(result.stdout.strip())


if __name__ == "__main__":
    main()
