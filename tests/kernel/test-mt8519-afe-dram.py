#!/usr/bin/env python3
"""Check that the MT8519 AFE gets its buffers at its own DRAM address (patch 0234).

The AFE reaches DRAM over the audio DSP's bus, which maps it at another base
than the CPU does, once the bus is told to. This checks the device tree carries
the two windows and the bus's configuration, that hw_params sets the window
before it programs translated addresses, and compiles both to check them: the
address the unit showed untranslated (UL9 at 0x429c0000), the values Amazon's
DSP driver writes, and a window that doesn't read back.
"""
from pathlib import Path
import os
import re
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


def main():
    linux = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    afe = (linux / "sound/soc/mediatek/mt8519/mt8519-afe-pcm.c").read_text()
    soc = (linux / "arch/arm/boot/dts/mediatek/mt8519.dtsi").read_text()

    node = soc[soc.index("adsp: adsp@10680000"):]
    node = node[:node.index("};")]
    regs = re.findall(r"<0 (0x[0-9a-f]+) 0 (0x[0-9a-f]+)>", re.search(r"reg = (.*?);", node, re.S).group(1))
    names = re.findall(r'"([^"]+)"', re.search(r"reg-names = (.*?);", node, re.S).group(1))
    window = dict(zip(names, (int(base, 16) for base, _ in regs)))
    if window.get("cpudram") != 0x40000000 or window.get("dspdram") != 0x50000000:
        raise SystemExit(f"The ADSP node's DRAM windows changed: {window}")

    node = soc[soc.index("afe: audio-controller@10610000"):]
    if "<0 0x1068f000 0 0x1000>" not in re.search(r"reg = (.*?);", node, re.S).group(1):
        raise SystemExit("The AFE node doesn't map the DSP bus's configuration (0x1068f000)")

    hw = afe[afe.index("static int mt8519_afe_fe_hw_params("):]
    hw = hw[:hw.index("\n}\n")]
    if "mt8519_afe_bus_addr(afe," not in hw:
        raise SystemExit("hw_params programs the memory interfaces with untranslated addresses")
    if not 0 <= hw.find("mt8519_afe_set_dram_window(afe)") < hw.find("mt8519_alloc_dmabuf("):
        raise SystemExit("hw_params must set the DSP bus's window before the buffer is programmed")
    if "mt8519_afe_read_dram_window(afe);" not in afe:
        raise SystemExit("The AFE doesn't read its DRAM window at probe")

    code = r'''
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <string.h>
typedef unsigned int u32;
typedef unsigned long long dma_addr_t;
typedef unsigned long long phys_addr_t;
#define __iomem
#define BIT(n) (1U << (n))
#define SZ_256M 0x10000000
#define dev_err(dev, ...) fprintf(stderr, __VA_ARGS__)
/* The bus's configuration page; the remap register keeps only bit 0. */
static unsigned char bus_page[0x1000];
static int stuck;
static void writel(u32 v, void *a)
{
    if (stuck)
        return;
    if ((unsigned char *)a == bus_page + 0x110)
        v &= 1;
    memcpy(a, &v, 4);
}
static u32 readl(void *a) { u32 v; memcpy(&v, a, 4); return v; }
struct device { int unused; };
struct mt8519_afe_private { phys_addr_t dram_cpu_base, dram_bus_base; void __iomem *dsp_bus; };
struct mtk_base_afe { struct mt8519_afe_private *platform_priv; struct device *dev; };
''' + "\n".join(re.findall(r"^#define MT8519_DSP_\w+.*$", afe, re.M)) + "\n" \
      + function_body(afe, "static dma_addr_t mt8519_afe_bus_addr(") + "\n" \
      + function_body(afe, "static int mt8519_afe_set_dram_window(") + r'''
int main(void)
{
    struct device dev;
    struct mt8519_afe_private priv = { 0x40000000, 0x50000000, bus_page };
    struct mtk_base_afe afe = { &priv, &dev };
    u32 v;

    /* Amazon's adsp_memory_remap_init(): remap on, EMI map 0. */
    memset(bus_page, 0xa5, sizeof(bus_page));
    assert(mt8519_afe_set_dram_window(&afe) == 0);
    memcpy(&v, bus_page + 0x110, 4);
    assert(v == 1);
    memcpy(&v, bus_page + 0xa00, 4);
    assert(v == 0);
    /* A window that doesn't take fails the stream. */
    stuck = 1;
    memset(bus_page, 0, sizeof(bus_page));
    assert(mt8519_afe_set_dram_window(&afe) == -EIO);
    stuck = 0;

    assert(mt8519_afe_bus_addr(&afe, 0x429c0000) == 0x529c0000);
    assert(mt8519_afe_bus_addr(&afe, 0x40000000) == 0x50000000);
    assert(mt8519_afe_bus_addr(&afe, 0x5fffffff) == 0x6fffffff);
    /* Below DRAM (the AFE's own SRAM) passes unchanged. */
    assert(mt8519_afe_bus_addr(&afe, 0x10620000) == 0x10620000);
    priv.dram_bus_base = 0;
    assert(mt8519_afe_bus_addr(&afe, 0x429c0000) == 0x429c0000);
    assert(mt8519_afe_set_dram_window(&afe) == 0);
    puts("ok: the DSP bus maps DRAM at +0x10000000 and the AFE's buffers are there");
    return 0;
}
'''
    with tempfile.TemporaryDirectory(prefix="pinna-mt8519-afe-dram-") as tmp:
        source = Path(tmp) / "dram.c"
        executable = Path(tmp) / "dram"
        source.write_text(code)
        subprocess.run([os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Werror",
                        str(source), "-o", str(executable)], check=True)
        subprocess.run([str(executable)], check=True)


if __name__ == "__main__":
    main()
