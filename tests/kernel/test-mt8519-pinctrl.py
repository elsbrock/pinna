#!/usr/bin/env python3
"""Run the MT8519 pin field setter against the I2C RSEL selections."""
from pathlib import Path
import os
import re
import subprocess
import sys
import tempfile


def section(text, start, end):
    return text[text.index(start):text.index(end, text.index(start))]


def main():
    linux = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    dtsi = (linux / "arch/arm/boot/dts/mediatek/mt8519-amzn.dtsi").read_text()
    common = (linux / "drivers/pinctrl/mediatek/pinctrl-mtk-common-v2.c").read_text()
    soc = (linux / "drivers/pinctrl/mediatek/pinctrl-mt8519.c").read_text()
    bindings = (linux / "include/dt-bindings/pinctrl/mt65xx.h").read_text()

    groups = []
    for match in re.finditer(r"\ti2c\d+_pin:\s*i2c\d+_pin\s*\{(.*?)\n\t\};", dtsi, re.S):
        block = match.group(1)
        bias = re.search(r"bias-pull-up\s*=\s*<\s*(MTK_PULL_SET_RSEL_\d{3})\s*>;", block)
        pinmux = re.search(r"pinmux\s*=\s*<(.*?)>;", block, re.S)
        if bias and pinmux:
            pins = [int(pin) for pin in re.findall(r"PINMUX_GPIO(\d+)__", pinmux.group(1))]
            groups.append((bias.group(1), pins))
    if len(groups) != 5 or any(token != "MTK_PULL_SET_RSEL_011" or len(pins) != 2 for token, pins in groups):
        raise SystemExit(f"Expected five corrected I2C RSEL pairs, found {groups}")

    field_table = section(soc, "static const struct mtk_pin_field_calc mt8519_pin_rsel_range[] = {", "};")
    field_entries = {}
    for row in re.findall(r"PIN_FIELD_BASE\(([^)]+)\)", field_table):
        values = [int(value.strip(), 0) for value in row.split(",")]
        start_pin, end_pin, index, offset, _stride, bitpos, width = values
        for pin in range(start_pin, end_pin + 1):
            field_entries[pin] = (index, offset, bitpos, (1 << width) - 1, width)
    dts_pins = [pin for _, pins in groups for pin in pins]
    if len(set(dts_pins)) != 10 or any(pin not in field_entries or field_entries[pin][4] != 2 for pin in dts_pins):
        raise SystemExit("I2C pins do not resolve to the expected two-bit MT8519 RSEL fields")

    enum_defs = "\n".join(line for line in bindings.splitlines()
                           if re.match(r"#define MTK_PULL_SET_RSEL_(000|011|111)\s+", line))
    if len(enum_defs.splitlines()) != 3:
        raise SystemExit("Required RSEL binding values are missing")
    setter = section(common, "int mtk_hw_set_value(", "EXPORT_SYMBOL_GPL(mtk_hw_set_value);")
    c_fields = ",\n".join(
        f"    {{ {pin}, {index}, 0x{offset:x}, {mask}, {bitpos} }}"
        for pin, (index, offset, bitpos, mask, _width) in sorted(field_entries.items())
        if pin in set(dts_pins)
    )
    c_tests = ",\n".join(
        f"    {{ {pin}, {token} - MTK_PULL_SET_RSEL_000 }}"
        for token, pins in groups for pin in pins
    )

    harness = r'''
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
typedef uint8_t u8;
typedef uint32_t u32;
'''
    harness += enum_defs + "\n"
    harness += r'''
struct mtk_pinctrl { u32 registers[8][256]; unsigned int calls; u32 last_mask, last_set; };
struct mtk_pin_desc { unsigned int number; };
struct mtk_pin_field { u8 index; u32 offset, mask; u8 bitpos, next; };
struct field_meta { unsigned int pin; u8 index; u32 offset, mask; u8 bitpos; };
static const struct field_meta field_metas[] = {
'''
    harness += c_fields + r'''
};
static int mtk_hw_pin_field_get(struct mtk_pinctrl *hw, const struct mtk_pin_desc *desc,
                                int field, struct mtk_pin_field *pf)
{
    (void)hw; (void)field;
    for (unsigned int i = 0; i < sizeof(field_metas) / sizeof(field_metas[0]); ++i) {
        if (field_metas[i].pin == desc->number) {
            pf->index = field_metas[i].index;
            pf->offset = field_metas[i].offset;
            pf->bitpos = field_metas[i].bitpos;
            pf->mask = field_metas[i].mask;
            pf->next = 0;
            return 0;
        }
    }
    return -EINVAL;
}
static void mtk_rmw(struct mtk_pinctrl *hw, unsigned int index, unsigned int offset,
                    u32 mask, u32 set)
{
    hw->last_mask = mask;
    hw->last_set = set;
    hw->registers[index][offset / 4] = (hw->registers[index][offset / 4] & ~mask) | set;
    ++hw->calls;
}
static void mtk_hw_write_cross_field(struct mtk_pinctrl *hw, struct mtk_pin_field *pf, int value)
{
    (void)hw; (void)pf; (void)value;
    assert(!"the I2C RSEL fields in this test do not cross registers");
}
'''
    harness += setter + "\n"
    harness += r'''
struct i2c_rsel_test { unsigned int pin; int corrected; };
static const struct i2c_rsel_test i2c_tests[] = {
'''
    harness += c_tests + r'''
};
int main(void)
{
    struct mtk_pinctrl hw = { 0 };
    assert(sizeof(i2c_tests) / sizeof(i2c_tests[0]) == 10);
    for (unsigned int i = 0; i < sizeof(i2c_tests) / sizeof(i2c_tests[0]); ++i) {
        struct mtk_pin_desc desc = { .number = i2c_tests[i].pin };
        struct mtk_pin_field pf;
        assert(mtk_hw_pin_field_get(&hw, &desc, 0, &pf) == 0);
        int old_bsp_value = MTK_PULL_SET_RSEL_111 - MTK_PULL_SET_RSEL_000;
        assert(old_bsp_value == 7);
        if ((old_bsp_value & pf.mask) != (unsigned int)i2c_tests[i].corrected) { fprintf(stderr, "pin %u mask %u old %d corrected %d\n", i2c_tests[i].pin, pf.mask, old_bsp_value, i2c_tests[i].corrected); return 1; }
        hw.registers[pf.index][pf.offset / 4] = 0xa5a5a5a5;
        hw.calls = 0;
        assert(mtk_hw_set_value(&hw, &desc, 0, old_bsp_value) == -EINVAL);
        assert(hw.calls == 0);
        assert(mtk_hw_set_value(&hw, &desc, 0, i2c_tests[i].corrected) == 0);
        assert(hw.calls == 1);
        assert(hw.last_mask == (pf.mask << pf.bitpos));
        assert(hw.last_set == (i2c_tests[i].corrected << pf.bitpos));
        assert(((hw.registers[pf.index][pf.offset / 4] >> pf.bitpos) & pf.mask) ==
               i2c_tests[i].corrected);
        assert((hw.registers[pf.index][pf.offset / 4] & ~hw.last_mask) ==
               (0xa5a5a5a5 & ~hw.last_mask));
    }
    puts("MT8519 I2C RSEL field validation and effective value: PASS");
    return 0;
}
'''
    with tempfile.TemporaryDirectory(prefix="pinna-mt8519-pinctrl-") as tmp:
        source = Path(tmp) / "pinctrl-test.c"
        executable = Path(tmp) / "pinctrl-test"
        source.write_text(harness)
        subprocess.run([os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Wextra", "-Werror", "-Wno-sign-compare",
                        str(source), "-o", str(executable)], check=True)
        subprocess.run([str(executable)], check=True)


if __name__ == "__main__":
    main()
