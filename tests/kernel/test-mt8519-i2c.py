#!/usr/bin/env python3
"""Exercise the MT8519 I2C data and helpers from the patched C driver."""
from pathlib import Path
import os
import subprocess
import sys
import tempfile


def section(text, start, end):
    return text[text.index(start):text.index(end, text.index(start))]


def main():
    source = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    driver = source / "drivers/i2c/busses/i2c-mt65xx.c"
    text = driver.read_text()
    if '.compatible = "mediatek,mt8519-i2c", .data = &mt8519_compat' not in text:
        raise SystemExit("MT8519 compatible is missing from the driver match table")

    constants = "\n".join(line for line in text.splitlines()
                           if line.startswith("#define I2C_FIFO_ADDR_CLR"))
    enum = section(text, "enum I2C_REGS_OFFSET {", "};") + "};"
    regs = section(text, "static const u16 mt_i2c_regs_v3[] = {", "};") + "};"
    compat_struct = section(text, "struct mtk_i2c_compatible {", "};") + "};"
    compat = section(text, "static const struct mtk_i2c_compatible mt8519_compat = {", "};") + "};"
    byte_accessors = section(text, "static u8 mtk_i2c_readb(", "static u16 mtk_i2c_readw(")
    helpers = section(text, "static bool mtk_i2c_use_dma(", "static int mtk_i2c_do_transfer(")

    harness = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
typedef uint8_t u8;
typedef uint16_t u16;
#define BIT(n) (1U << (n))
#define I2C_CONTROL_DMA_EN (1U << 2)
#define I2C_CONTROL_DMAACK_EN (1U << 8)
#define I2C_CONTROL_ASYNC_MODE (1U << 9)
'''
    harness += constants + "\n" + enum + "\n" + regs + "\n" + compat_struct + "\n" + compat + "\n"
    harness += r'''
enum mtk_trans_op { I2C_MASTER_WR = 1, I2C_MASTER_RD, I2C_MASTER_WRRD };
struct i2c_msg { u16 len; u8 *buf; };
struct mtk_i2c {
    const struct mtk_i2c_compatible *dev_comp;
    enum mtk_trans_op op;
    bool enable_multi_user;
    u8 base[256];
    u8 written[32];
    u8 read_data[32];
    unsigned int writes, reads;
};
static struct mtk_i2c *active_i2c;
static void writeb(u8 val, u8 *address)
{
    assert(address - active_i2c->base == active_i2c->dev_comp->regs[OFFSET_DATA_PORT]);
    active_i2c->written[active_i2c->writes++] = val;
}
static u8 readb(u8 *address)
{
    assert(address - active_i2c->base == active_i2c->dev_comp->regs[OFFSET_DATA_PORT]);
    return active_i2c->read_data[active_i2c->reads++];
}
'''
    harness += byte_accessors + "\n" + helpers
    harness += r'''
int main(void)
{
    assert(mt8519_compat.regs == mt_i2c_regs_v3);
    assert(mt8519_compat.regs[OFFSET_SLAVE_ADDR] == 0x94);
    assert(mt8519_compat.pmic_i2c == 0 && mt8519_compat.dcm == 0);
    assert(mt8519_compat.auto_restart && mt8519_compat.aux_len_reg);
    assert(mt8519_compat.timing_adjust && !mt8519_compat.dma_sync);
    assert(mt8519_compat.ltiming_adjust && !mt8519_compat.apdma_sync);
    assert(mt8519_compat.max_dma_support == 36 && mt8519_compat.fifo_size == 16);
    assert(I2C_FIFO_ADDR_CLR == 0x1 && I2C_FIFO_ADDR_CLR_MCH == 0x4);

    struct mtk_i2c i2c = { .dev_comp = &mt8519_compat };
    active_i2c = &i2c;
    struct i2c_msg msg[2] = { { .len = 16 }, { .len = 16 } };
    for (int op = I2C_MASTER_WR; op <= I2C_MASTER_WRRD; ++op) {
        i2c.op = op;
        assert(!mtk_i2c_use_dma(&i2c, msg));
        msg[0].len = 17;
        assert(mtk_i2c_use_dma(&i2c, msg));
        msg[0].len = 16;
        msg[1].len = 17;
        assert(mtk_i2c_use_dma(&i2c, msg) == (op == I2C_MASTER_WRRD));
        msg[1].len = 16;
    }
    struct mtk_i2c_compatible legacy = { .fifo_size = 0 };
    i2c.dev_comp = &legacy;
    i2c.op = I2C_MASTER_WR;
    msg[0].len = 1;
    assert(mtk_i2c_use_dma(&i2c, msg));
    i2c.dev_comp = &mt8519_compat;

    u8 outgoing[] = { 0x12, 0x34 };
    u8 read_single[2] = { 0 };
    u8 read_combined[3] = { 0 };
    struct i2c_msg io_msgs[2] = {
        { .len = sizeof(outgoing), .buf = outgoing },
        { .len = sizeof(read_combined), .buf = read_combined },
    };
    i2c.read_data[0] = 0xa1; i2c.read_data[1] = 0xb2;
    i2c.read_data[2] = 0xc3; i2c.read_data[3] = 0xd4; i2c.read_data[4] = 0xe5;

    i2c.op = I2C_MASTER_WR;
    mtk_i2c_pio_write(&i2c, io_msgs);
    mtk_i2c_pio_read(&i2c, io_msgs);
    assert(i2c.writes == 2 && i2c.written[0] == 0x12 && i2c.written[1] == 0x34);
    assert(i2c.reads == 0);

    io_msgs[0].len = sizeof(read_single);
    io_msgs[0].buf = read_single;
    i2c.op = I2C_MASTER_RD;
    mtk_i2c_pio_write(&i2c, io_msgs);
    mtk_i2c_pio_read(&i2c, io_msgs);
    assert(i2c.writes == 2 && i2c.reads == 2);
    assert(read_single[0] == 0xa1 && read_single[1] == 0xb2);

    io_msgs[0].len = sizeof(outgoing);
    io_msgs[0].buf = outgoing;
    io_msgs[1].len = sizeof(read_combined);
    io_msgs[1].buf = read_combined;
    i2c.op = I2C_MASTER_WRRD;
    mtk_i2c_pio_write(&i2c, io_msgs);
    mtk_i2c_pio_read(&i2c, io_msgs);
    assert(i2c.writes == 4 && i2c.written[2] == 0x12 && i2c.written[3] == 0x34);
    assert(i2c.reads == 5);
    assert(read_combined[0] == 0xc3 && read_combined[1] == 0xd4 && read_combined[2] == 0xe5);

    assert(mtk_i2c_fifo_clear_value(false) == 0x1);
    assert(mtk_i2c_fifo_clear_value(true) == 0x4);
    u16 dma_bits = I2C_CONTROL_DMA_EN | I2C_CONTROL_DMAACK_EN | I2C_CONTROL_ASYNC_MODE;
    assert(mtk_i2c_set_dma_control(dma_bits | 1, false, false) == 1);
    assert(mtk_i2c_set_dma_control(1, true, false) == (1 | I2C_CONTROL_DMA_EN));
    assert(mtk_i2c_set_dma_control(1, true, true) == (1 | dma_bits));
    puts("MT8519 I2C data, FIFO accessors, DMA boundaries and control: PASS");
    return 0;
}
'''
    with tempfile.TemporaryDirectory(prefix="pinna-mt8519-i2c-") as tmp:
        test = Path(tmp) / "i2c-test.c"
        executable = Path(tmp) / "i2c-test"
        test.write_text(harness)
        subprocess.run([os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Wextra", "-Werror",
                        str(test), "-o", str(executable)], check=True)
        subprocess.run([str(executable)], check=True)


if __name__ == "__main__":
    main()
