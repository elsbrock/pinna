#!/usr/bin/env python3
"""Run the patched MediaTek SRAM functions against a recording regmap.

On your computer, after building/applying the kernel patches:
    python3 tests/kernel/test-mt8519-sram.py [path/to/linux]
Requires a native C compiler (cc). Does not access hardware.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    source = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    driver = source / "drivers/pmdomain/mediatek"
    text = (driver / "mtk-pm-domains.c").read_text()
    start = text.index("static int scpsys_sram_enable(")
    end = text.index("static struct regmap *scpsys_bus_protect_get_regmap(", start)
    functions = text[start:end]
    harness = r'''
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
#define BIT(n) (1U << (n))
#define GENMASK(h, l) ((~0U << (l)) & (~0U >> (31 - (h))))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#include "mtk-pm-domains.h"
/* Bus protection is not part of the SRAM functions under test. */
#undef BUS_PROT_WR
#define BUS_PROT_WR(...) { 0 }
#include "mt8519-pm-domains.h"
#define PWR_SRAM_CLKISO_BIT BIT(5)
#define PWR_SRAM_ISOINT_B_BIT BIT(6)
struct regmap {
    unsigned int expected, pdn, ack, value, writes, reads;
    int stuck;
};
struct scpsys { struct regmap *base; };
struct scpsys_domain {
    const struct scpsys_domain_data *data;
    struct scpsys *scpsys;
};
static void regmap_set_bits(struct regmap *map, unsigned int offset, unsigned int mask)
{
    assert(offset == map->expected);
    map->value |= mask;
    ++map->writes;
}
static void regmap_clear_bits(struct regmap *map, unsigned int offset, unsigned int mask)
{
    assert(offset == map->expected);
    map->value &= ~mask;
    ++map->writes;
}
static unsigned int mock_read(struct regmap *map, unsigned int offset)
{
    assert(offset == map->expected);
    ++map->reads;
    if (map->stuck)
        return map->value;
    return (map->value & map->pdn) ? map->value | map->ack : map->value & ~map->ack;
}
#define regmap_read_poll_timeout(map, offset, value, condition, delay, timeout) \
    ({ (value) = mock_read((map), (offset)); (condition) ? 0 : -110; })
static void udelay(unsigned int delay) { assert(delay == 1); }
'''
    harness += functions
    harness += r'''
static void check_domain(const struct scpsys_domain_data *data, unsigned int expected)
{
    struct regmap map = { .expected = expected, .pdn = data->sram_pdn_bits,
                          .ack = data->sram_pdn_ack_bits };
    struct scpsys sys = { .base = &map };
    struct scpsys_domain pd = { .data = data, .scpsys = &sys };
    unsigned int writes = data->caps & MTK_SCPD_SRAM_ISO ? 3 : 1;

    map.value = map.pdn | map.ack | BIT(31);
    assert(scpsys_sram_enable(&pd) == 0);
    assert(map.writes == writes && map.reads == 1);
    assert(!(map.value & map.pdn) && (map.value & BIT(31)));
    map.writes = map.reads = 0;
    map.value = BIT(31);
    assert(scpsys_sram_disable(&pd) == 0);
    assert(map.writes == writes && map.reads == 1);
    assert((map.value & map.pdn) == map.pdn && (map.value & BIT(31)));

    if (map.ack) {
        map.stuck = 1;
        map.value = map.pdn | map.ack;
        map.writes = map.reads = 0;
        assert(scpsys_sram_enable(&pd) == -110);
        assert(map.writes == 1 && map.reads == 1);
        map.value = 0;
        assert(scpsys_sram_disable(&pd) == -110);
    }
}
int main(void)
{
    const struct scpsys_domain_data *adsp =
        &scpsys_domain_data_mt8519[MT8519_POWER_DOMAIN_ADSP_INFRA];
    /* Amazon's MT8519 BSP, Cannoli 5.4.86: distinct power and SRAM registers. */
    assert(adsp->ctl_offs == 0xe2c);
    assert(adsp->sram_ctl_offs == 0xea4);
    assert(adsp->sram_pdn_bits == BIT(16));
    assert(adsp->sram_pdn_ack_bits == BIT(24));
    for (unsigned int i = 0; i < sizeof(scpsys_domain_data_mt8519) / sizeof(*adsp); ++i) {
        const struct scpsys_domain_data *data = &scpsys_domain_data_mt8519[i];
        unsigned int expected = i == MT8519_POWER_DOMAIN_ADSP_INFRA ? 0xea4 : data->ctl_offs;
        if (i != MT8519_POWER_DOMAIN_ADSP_INFRA)
            assert(!data->sram_ctl_offs);
        check_domain(data, expected);
    }
    /* Existing SoCs use ctl_offs. Exercise their isolation and ACK paths too. */
    struct scpsys_domain_data legacy = { .ctl_offs = 0x210, .sram_pdn_bits = BIT(8),
        .sram_pdn_ack_bits = BIT(12), .caps = MTK_SCPD_SRAM_ISO };
    check_domain(&legacy, 0x210);
    legacy.sram_ctl_offs = 0xea4;
    check_domain(&legacy, 0xea4);
    puts("MT8519 SRAM enable/disable, ACK failures and legacy fallback: PASS");
    return 0;
}
'''
    with tempfile.TemporaryDirectory(prefix="pinna-sram-test-") as temporary:
        folder = Path(temporary)
        test = folder / "sram.c"
        test.write_text(harness)
        executable = folder / "sram-test"
        subprocess.run([os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Wextra", "-Werror",
                        "-I" + str(driver), "-I" + str(source / "include"),
                        str(test), "-o", str(executable)], check=True)
        subprocess.run([str(executable)], check=True)


if __name__ == "__main__":
    main()
