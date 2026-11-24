#!/usr/bin/env python3
"""Exercise TSL2540 helpers and the Cannoli DT contract.

On your computer, after applying the kernel patches:
    python3 tests/kernel/test-mt8519-light.py [path/to/linux]

Requires cc. Does not access hardware.
"""
from pathlib import Path
import os
import re
import subprocess
import sys
import tempfile


def function(text, start, end):
    begin = text.index(start)
    finish = text.index(end, begin)
    return text[begin:finish]


def main():
    root = Path(__file__).resolve().parents[2]
    source = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    driver = source / "drivers/iio/light/tsl2540.c" if source.is_dir() else source
    text = driver.read_text()

    stock_compatible = "ams,tsl2540"
    stock_dts = (root / "kernel/patches/0204-arm-dts-mediatek-add-the-Echo-Dot-5th-gen-cannoli-de.patch").read_text()
    stock_compatibles = re.findall(r'\+\s*compatible\s*=\s*"([^"]+)"', stock_dts)
    if stock_compatible not in stock_compatibles:
        raise SystemExit("stock Cannoli device tree no longer declares the expected compatible")
    if "als_name" in stock_dts or "als_name" in text:
        raise SystemExit("TSL2540 DT contract unexpectedly requires the legacy als_name property")

    of_table = function(text, "static const struct of_device_id tsl2540_of_match[]", "};")
    active_compatibles = re.findall(r'\.compatible\s*=\s*"([^"]+)"', of_table)
    legacy_patch = (root / "kernel/patches/0013-iio-light-AMS-TSL2540-and-TSL2583-ambient-light-sens.patch").read_text()
    legacy_compatibles = re.findall(r'\+\s*\{ \.compatible = "([^"]+)"', legacy_patch)
    if stock_compatible not in active_compatibles:
        raise SystemExit("BSP driver does not match the stock Cannoli TSL2540 compatible")
    if stock_compatible in legacy_compatibles or "amstaos,tsl2540" not in legacy_compatibles:
        raise SystemExit("legacy AMS driver's mismatched compatible contract changed")

    config = (root / "kernel/mt8519.config").read_text()
    if "CONFIG_IIO_TSL2540=y" not in config:
        raise SystemExit("MT8519 does not enable the BSP TSL2540 IIO driver")
    if "# CONFIG_SENSORS_TSL2540 is not set" not in config:
        raise SystemExit("MT8519 still selects the mismatched legacy AMS driver")

    ids = "\n".join(line for line in text.splitlines()
                     if line.startswith("#define TSL2540_ID_"))
    read_helper = function(text, "static int\ntsl2540_i2c_read_byte(", "\n}\n") + "\n}"
    id_helper = function(text, "static int tsl2540_device_id(", "\n}\n") + "\n}"
    harness = r'''
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
typedef uint8_t u8;
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define I2C_M_RD 1
struct i2c_adapter { int unused; };
struct i2c_client { uint16_t addr; unsigned short flags; struct i2c_adapter *adapter; };
struct i2c_msg { uint16_t addr; unsigned short flags; uint16_t len; u8 *buf; };
static int transfer_result;
static u8 response_byte;
static int i2c_transfer(struct i2c_adapter *adapter, struct i2c_msg *msgs, int count)
{
    (void)adapter;
    assert(count == 2);
    assert(msgs[0].len == 1 && msgs[1].len == 1);
    assert(msgs[0].buf[0] == 0x92);
    assert(msgs[1].flags & I2C_M_RD);
    if (transfer_result == 2)
        msgs[1].buf[0] = response_byte;
    return transfer_result;
}
'''
    harness += ids + "\n" + read_helper + "\n" + id_helper
    harness += r'''
int main(void)
{
    struct i2c_adapter adapter = {0};
    struct i2c_client client = { .addr = 0x39, .adapter = &adapter };
    u8 value = 0;

    assert(tsl2540_device_id(0xe7, 0x79, 0x01));
    assert(!tsl2540_device_id(0xe0, 0x61, 0x01));
    assert(!tsl2540_device_id(0xe4, 0x62, 0x01));
    assert(!tsl2540_device_id(0xe4, 0x61, 0x00));

    transfer_result = 2;
    response_byte = 0xa5;
    assert(tsl2540_i2c_read_byte(&client, 0x92, &value) == 0);
    assert(value == 0xa5);
    transfer_result = -EREMOTEIO;
    assert(tsl2540_i2c_read_byte(&client, 0x92, &value) == -EREMOTEIO);
    transfer_result = 1;
    assert(tsl2540_i2c_read_byte(&client, 0x92, &value) == -EIO);
    transfer_result = 0;
    assert(tsl2540_i2c_read_byte(&client, 0x92, &value) == -EIO);
    puts("MT8519 TSL2540 ID, I2C error propagation and stock DT match: PASS");
    return 0;
}
'''

    with tempfile.TemporaryDirectory(prefix="pinna-mt8519-light-") as temporary:
        folder = Path(temporary)
        test = folder / "light-test.c"
        executable = folder / "light-test"
        test.write_text(harness)
        subprocess.run([os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Wextra",
                        "-Werror", str(test), "-o", str(executable)], check=True)
        subprocess.run([str(executable)], check=True)


if __name__ == "__main__":
    main()
