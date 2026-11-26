#!/usr/bin/env python3
"""Check the Echo Dot (5th gen)'s ADC clocking on mainline tlv320adcx140 (patch 0233).

The TLV320ADC5140 is the eTDM1 clock provider. This compiles the driver's
set_fmt, set_sysclk and hw_params against stubs that record register writes,
replays what the machine driver does on this board, and checks the registers
against what Amazon's ADC5140 driver leaves on the chip: MCLK 24.576 MHz,
96 kHz with 128 BCLKs a frame, provider mode, and BCLK not inverted.
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


def link_format(dts):
    """The eTDM1 link's format as the machine driver builds it from /sound."""
    def prop(name):
        m = re.search(r"mediatek,etdm1-in-" + name + r"\s*=\s*([^;]+);", dts)
        return m.group(1).strip() if m else None
    fmt = {"dsp_b": "SND_SOC_DAIFMT_DSP_B", "i2s": "SND_SOC_DAIFMT_I2S"}[prop("format").strip('"')]
    clock = "SND_SOC_DAIFMT_CBM_CFM" if prop("master-clock") == "<0>" else "SND_SOC_DAIFMT_CBS_CFS"
    inv = "SND_SOC_DAIFMT_IB_NF" if prop("bck-inverse") == "<1>" else "SND_SOC_DAIFMT_NB_NF"
    return f"{fmt} | {inv} | {clock}"


HARNESS = r'''
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>

typedef unsigned char u8;
#define BIT(n) (1U << (n))
#define GENMASK(h, l) ((~0U >> (31 - (h))) & (~0U << (l)))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define dev_err(dev, ...) fprintf(stderr, __VA_ARGS__)
#define SND_SOC_DAIFMT_FORMAT_MASK 0x000f
#define SND_SOC_DAIFMT_I2S 1
#define SND_SOC_DAIFMT_LEFT_J 3
#define SND_SOC_DAIFMT_DSP_A 4
#define SND_SOC_DAIFMT_DSP_B 5
#define SND_SOC_DAIFMT_INV_MASK 0x0f00
#define SND_SOC_DAIFMT_NB_NF (0 << 8)
#define SND_SOC_DAIFMT_NB_IF (2 << 8)
#define SND_SOC_DAIFMT_IB_NF (3 << 8)
#define SND_SOC_DAIFMT_IB_IF (4 << 8)
#define SND_SOC_DAIFMT_CLOCK_PROVIDER_MASK 0xf000
#define SND_SOC_DAIFMT_CBP_CFP (1 << 12)
#define SND_SOC_DAIFMT_CBC_CFC (4 << 12)
#define SND_SOC_DAIFMT_CBM_CFM SND_SOC_DAIFMT_CBP_CFP
#define SND_SOC_DAIFMT_CBS_CFS SND_SOC_DAIFMT_CBC_CFC
#define __fls(x) (31 - __builtin_clz(x))

static unsigned char regs[256];
struct device { int unused; };
struct snd_soc_component { struct device *dev; };
struct snd_soc_dai { struct snd_soc_component *component; struct device *dev; };
struct snd_pcm_substream { int unused; };
struct snd_pcm_hw_params { unsigned int rate, channels, width; };
#define params_rate(p) ((p)->rate)
#define params_channels(p) ((p)->channels)
#define params_physical_width(p) ((p)->width)
#define snd_soc_component_get_drvdata(c) (&priv)
static int snd_soc_component_update_bits(struct snd_soc_component *c, unsigned int reg,
                                         unsigned int mask, unsigned int val)
{
    (void)c;
    unsigned char old = regs[reg];
    regs[reg] = (old & ~mask) | (val & mask);
    return old != regs[reg];
}
static int snd_soc_component_write(struct snd_soc_component *c, unsigned int reg, unsigned int val)
{
    (void)c;
    regs[reg] = val;
    return 0;
}
'''


def main():
    linux = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    codec = (linux / "sound/soc/codecs/tlv320adcx140.c").read_text()
    header = (linux / "sound/soc/codecs/tlv320adcx140.h").read_text()
    machine = (linux / "sound/soc/mediatek/mt8519/mendocino.c").read_text()
    dts = (linux / "arch/arm/boot/dts/mediatek/cannoli-cheesecake.dtsi").read_text()

    adc = re.search(r"tlv320adc5140_1: tlv320adc5140@4c \{(.*?)\n\t\};", dts, re.S).group(1)
    if not re.search(r"ti,gpio-config\s*=\s*<10 2>;", adc):
        raise SystemExit("The ADC's GPIO1 is not the MCLK input (ti,gpio-config = <10 2>)")
    if "mt8519_mendocino_is_ti_adc" in machine or "fmt = rtd->dai_link->dai_fmt;" not in machine:
        raise SystemExit("The machine driver must hand the TI ADC the link's own BCLK polarity")
    if ".set_sysclk\t= adcx140_set_dai_sysclk" not in codec:
        raise SystemExit("tlv320adcx140 has no set_sysclk for MCLK's frequency")

    struct = re.search(r"struct adcx140_priv \{.*?\n\};", codec, re.S).group(0)
    struct = re.sub(r"struct (regulator|gpio_desc|regmap) \*\w+;", "", struct)
    struct = struct.replace("struct device *dev;", "")
    defines = "\n".join(l for l in header.splitlines() if l.startswith("#define ADCX140_"))
    defines += "\n" + "\n".join(re.findall(r"^#define ADCX140_(?:MCLK_FREQ_SEL_MSK|FS_RATE_SHIFT)\b.*$",
                                           codec, re.M))
    code = HARNESS + defines + "\n" + struct + "\nstatic struct adcx140_priv priv;\n"
    code += "static void adcx140_pwr_ctrl(struct adcx140_priv *a, bool on) { (void)a; (void)on; }\n"
    for sig in ("static int adcx140_set_master_rate(", "static int adcx140_hw_params(",
                "static int adcx140_set_dai_fmt(", "static int adcx140_set_dai_sysclk("):
        code += function_body(codec, sig) + "\n"
    code += r'''
int main(void)
{
    struct device dev;
    struct snd_soc_component component = { &dev };
    struct snd_soc_dai dai = { &component, &dev };
    struct snd_pcm_substream substream;
    struct snd_pcm_hw_params params = { 96000, 4, 32 };

    regs[ADCX140_MST_CFG0] = 0x02; /* reset values */
    regs[ADCX140_MST_CFG1] = 0x48;
    assert(adcx140_set_dai_fmt(&dai, LINK_FORMAT) == 0);
    assert(adcx140_set_dai_sysclk(&dai, 0, 24576000, 1) == 0);
    assert(adcx140_hw_params(&substream, &params, &dai) == 0);
    printf("MST_CFG0 %02x MST_CFG1 %02x ASI_CFG0 %02x\n",
           regs[ADCX140_MST_CFG0], regs[ADCX140_MST_CFG1], regs[ADCX140_ASI_CFG0]);
    /* Amazon's ADC5140 driver: provider, MCLK 24.576 MHz (7). */
    assert(regs[ADCX140_MST_CFG0] == (0x80 | 7));
    /* 96 kHz (5) and 128 BCLKs a frame (6). */
    assert(regs[ADCX140_MST_CFG1] == 0x56);
    /* TDM, 32-bit words, BCLK not inverted, as Amazon's driver left it. */
    assert((regs[ADCX140_ASI_CFG0] & ADCX140_BCLKINV_BIT) == 0);
    assert((regs[ADCX140_ASI_CFG0] & ADCX140_WORD_LEN_MSK) == ADCX140_32_BIT_WORD);
    assert(adcx140_set_dai_sysclk(&dai, 0, 11289600, 1) == -EINVAL);
    params.rate = 44100;
    assert(adcx140_hw_params(&substream, &params, &dai) == -EINVAL);
    puts("ok: the ADC is clock provider at 96 kHz, 128 BCLKs, MCLK 24.576 MHz, as Amazon's driver sets it");
    return 0;
}
'''
    with tempfile.TemporaryDirectory(prefix="pinna-mt8519-adc-") as tmp:
        source = Path(tmp) / "adc.c"
        executable = Path(tmp) / "adc"
        source.write_text(code)
        subprocess.run([os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Wno-unused-function",
                        "-Wno-unused-variable", f"-DLINK_FORMAT=({link_format(dts)})",
                        str(source), "-o", str(executable)], check=True)
        subprocess.run([str(executable)], check=True)


if __name__ == "__main__":
    main()
