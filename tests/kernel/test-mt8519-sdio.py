#!/usr/bin/env python3
"""Exercise the MT8519 host match, hardware setup and tuning selection."""
from pathlib import Path
import os
import re
import subprocess
import sys
import tempfile


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
    source = (linux / "drivers/mmc/host/mtk-sd.c").read_text()
    soc = (linux / "arch/arm/boot/dts/mediatek/mt8519.dtsi").read_text()
    board = (linux / "arch/arm/boot/dts/mediatek/mt8519-amzn.dtsi").read_text()
    compatible = re.search(r'compatible\s*=\s*"([^"]+)"', block(soc, "mmc1: sdio@11240000")).group(1)
    if not re.search(r'\.compatible\s*=\s*"' + re.escape(compatible) + '"', source):
        raise SystemExit(f"No MMC host match for the board's {compatible}")
    if "no-mmc;" not in block(board, "&mmc1 {"):
        raise SystemExit("The SDIO board must select the 64-step no-mmc tuning path")
    defines = "\n".join(re.findall(r"^#define[^\n]*(?:\\\n[^\n]*)*", source, re.M))
    compat = block(source, "struct mtk_mmc_compatible {") + ";\n"
    tables = source[source.index("static const struct mtk_mmc_compatible mt2701_compat"):source.index("MODULE_DEVICE_TABLE(of, msdc_of_ids)")]
    tune_data = block(source, "struct msdc_tune_para {") + ";\n"
    init = block(source, "static void msdc_init_hw(")
    tune = block(source, "static int msdc_execute_tuning(")
    selection = block(source, 'if (of_property_read_u32(pdev->dev.of_node, "mediatek,tuning-step",')
    c = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>
typedef uint8_t u8;
typedef uint32_t u32;
typedef uint64_t u64;
#define BIT(n) (1U << (n))
#define BIT_ULL(n) (1ULL << (n))
#define GENMASK(h, l) ((~0U >> (31 - (h))) & (~0U << (l)))
#define MMC_CAP2_NO_MMC BIT(0)
#define MMC_CAP2_NO_SDIO BIT(1)
#define dev_dbg(...) ((void)0)
#define dev_err(...) ((void)0)
'''
    c += defines + "\n" + compat + tune_data
    c += r'''
struct of_device_id { const char *compatible; const void *data; };
struct msdc_host;
struct mmc_host { unsigned int caps2; struct msdc_host *private; };
struct device { void *of_node; };
struct platform_device { struct device dev; };
struct msdc_host {
    void *base, *top_base, *dev, *reset;
    const struct mtk_mmc_compatible *dev_comp;
    struct mmc_host *mmc;
    unsigned int tuning_step;
    bool internal_cd, hs400_mode;
    struct msdc_tune_para def_tune_para, saved_tune_para;
};
static u32 readl(const void *reg) { return *(const u32 *)reg; }
static void writel(u32 value, void *reg) { *(u32 *)reg = value; }
static void sdr_set_bits(void *reg, u32 bits) { writel(readl(reg) | bits, reg); }
static void sdr_clr_bits(void *reg, u32 bits) { writel(readl(reg) & ~bits, reg); }
static void sdr_set_field(void *reg, u32 mask, u32 value)
{ writel((readl(reg) & ~mask) | (value << __builtin_ctz(mask)), reg); }
static struct mmc_host *mmc_from_priv(struct msdc_host *h) { return h->mmc; }
static struct msdc_host *mmc_priv(struct mmc_host *m) { return m->private; }
static void msdc_reset_hw(struct msdc_host *h) { (void)h; }
static void reset_control_assert(void *p) { (void)p; }
static void reset_control_deassert(void *p) { (void)p; }
static void usleep_range(unsigned int a, unsigned int b) { (void)a; (void)b; }
static unsigned int property_step;
static int of_property_read_u32(void *np, const char *name, unsigned int *value)
{
    (void)np;
    assert(strcmp(name, "mediatek,tuning-step") == 0);
    if (!property_step) return -ENOENT;
    *value = property_step; return 0;
}
static unsigned int together_calls, separate_calls;
static int tuning_result;
static int msdc_tune_together(struct mmc_host *m, u32 op)
{ (void)m; (void)op; ++together_calls; return tuning_result; }
static int hs400_tune_response(struct mmc_host *m, u32 op)
{ (void)m; (void)op; ++separate_calls; return 0; }
static int msdc_tune_response(struct mmc_host *m, u32 op)
{ (void)m; (void)op; ++separate_calls; return 0; }
static int msdc_tune_data(struct mmc_host *m, u32 op)
{ (void)m; (void)op; ++separate_calls; return 0; }
static void msdc_set_data_delay(struct msdc_host *h, u32 d) { (void)h; (void)d; }
'''
    c += tables + init + "\n" + tune
    c += "\nstatic void select_step(struct msdc_host *host) {\nstruct mmc_host *mmc = host->mmc;\nstruct platform_device p = {0}, *pdev = &p;\n" + selection + "\n}\n"
    c += r'''
int main(void)
{
    const struct mtk_mmc_compatible *variant = NULL;
    for (unsigned int i = 0; msdc_of_ids[i].compatible; ++i)
        if (!strcmp(msdc_of_ids[i].compatible, BOARD_COMPATIBLE))
            variant = msdc_of_ids[i].data;
    assert(variant);
    /* Amazon's MT8519 compatibility data, not an existing SoC fallback. */
    assert(variant->clk_div_bits == 12 && variant->pad_tune_reg == MSDC_PAD_TUNE0);
    assert(variant->async_fifo && variant->data_tune && variant->busy_check);
    assert(variant->stop_clk_fix && variant->enhance_rx && !variant->support_64g);
    assert(!variant->hs400_tune);
    for (unsigned int top = 0; top < 2; ++top) {
        u32 base[1024] = {0}, upper[1024] = {0};
        struct msdc_host host = { .base = base, .top_base = top ? upper : NULL,
                                  .dev_comp = variant };
        struct mmc_host mmc = { .caps2 = MMC_CAP2_NO_MMC, .private = &host };
        host.mmc = &mmc;
        property_step = 0;
        select_step(&host);
        assert(host.tuning_step == 64);
        msdc_init_hw(&host);
        assert(readl(host.base + SDC_CFG) & SDC_CFG_SDIO);
        assert(!(readl(host.base + MSDC_PATCH_BIT2) & MSDC_PB2_SUPPORT_64G));
        assert(readl(host.base + MSDC_PATCH_BIT1) & BIT(3)); /* BSP v3_plus=false */
        if (top) {
            assert(readl(host.top_base + EMMC_TOP_CONTROL) & SDC_RX_ENH_EN);
            assert(readl(host.top_base + EMMC_TOP_CONTROL) & PAD_DAT_RD_RXDLY2_SEL);
            assert(readl(host.top_base + EMMC_TOP_CMD) & PAD_CMD_RD_RXDLY2_SEL);
        } else {
            assert(readl(host.base + SDC_ADV_CFG0) & SDC_RX_ENHANCE_EN);
            assert(readl(host.base + variant->pad_tune_reg + TUNING_REG2_FIXED_OFFEST)
                   & MSDC_PAD_TUNE_CMD2_SEL);
        }
        together_calls = separate_calls = 0;
        tuning_result = 0;
        assert(msdc_execute_tuning(&mmc, 19) == 0);
        assert(together_calls == 1 && separate_calls == 0);
        tuning_result = -EIO;
        assert(msdc_execute_tuning(&mmc, 19) == -EIO);
        mmc.caps2 = 0;
        select_step(&host);
        assert(host.tuning_step == 32);
        mmc.caps2 = MMC_CAP2_NO_MMC;
        property_step = 32;
        select_step(&host);
        assert(host.tuning_step == 32);
    }
    puts("MT8519 SDIO match, hardware setup and tuning selection: PASS");
    return 0;
}
'''
    c = '#define BOARD_COMPATIBLE "' + compatible + '"\n' + c
    with tempfile.TemporaryDirectory(prefix="pinna-mt8519-sdio-") as tmp:
        path = Path(tmp) / "sdio.c"
        exe = Path(tmp) / "sdio"
        path.write_text(c)
        subprocess.run([os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Wextra", "-Werror",
                        str(path), "-o", str(exe)], check=True)
        subprocess.run([str(exe)], check=True)


if __name__ == "__main__":
    main()
