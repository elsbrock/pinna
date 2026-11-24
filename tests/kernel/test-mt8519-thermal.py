#!/usr/bin/env python3
"""Compile the actual thermal IRQ block against lazy DT IRQ lookup mocks.

On your computer, after applying the kernel patches:
    python3 tests/kernel/test-mt8519-thermal.py [path/to/linux/or/soc_temp.c]
Requires cc. Does not access hardware.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    source = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux")
    if source.is_dir():
        source /= "drivers/thermal/mediatek/soc_temp.c"
    text = source.read_text()
    anchor = "\tirq = platform_get_irq(pdev, 0);\n"
    if anchor not in text:
        anchor = "\tres = platform_get_resource(pdev, IORESOURCE_IRQ, 0);\n"
    start = text.index(anchor, text.index("static int soc_thermal_probe("))
    end = text.index("\tsoc_thermal_debugfs(pdev);\n", start)
    harness = r'''
#include <assert.h>
#include <errno.h>
#include <stddef.h>
#include <stdio.h>
#define EPROBE_DEFER 517
#define IRQF_TRIGGER_HIGH 4
#define IORESOURCE_IRQ 1
#define dev_err(...) ((void)0)
struct platform_device { int dev; };
struct resource { int start; };
struct soc_thermal { int unused; };
static int lookup_result, request_result, lookup_calls, request_calls, requested;
static void irq_handler(void) {}
static int __attribute__((unused)) platform_get_irq(struct platform_device *dev, int index)
{
    (void)dev;
    assert(index == 0);
    ++lookup_calls;
    return lookup_result;
}
/* OF's resource table contains no IRQ entries in 6.12. */
static __attribute__((unused)) struct resource *platform_get_resource(
    struct platform_device *dev, int type, int index)
{
    (void)dev; (void)type; (void)index;
    return NULL;
}
static int devm_request_irq(void *dev, int irq, void (*handler)(void),
                            int flags, const char *name, void *data)
{
    (void)dev; (void)name; (void)data;
    assert(handler == irq_handler && flags == IRQF_TRIGGER_HIGH);
    ++request_calls;
    requested = irq;
    return request_result;
}
static int run(struct platform_device *pdev, struct soc_thermal *mt)
{
    int irq = 0, ret = 0;
    struct resource *res = NULL;
    (void)irq; (void)res;
    if (0) goto err_disable_clk_peri_therm;
'''
    harness += text[start:end]
    harness += r'''
    return 0;
err_disable_clk_peri_therm:
    return ret;
}
static void check(int lookup, int request, int expected, int calls)
{
    struct platform_device dev = {0};
    struct soc_thermal thermal = {0};
    lookup_result = lookup;
    request_result = request;
    lookup_calls = request_calls = requested = 0;
    assert(run(&dev, &thermal) == expected);
    assert(lookup_calls == 1 && request_calls == calls);
    if (calls) assert(requested == lookup);
}
int main(void)
{
    check(55, 0, 0, 1);
    check(-EPROBE_DEFER, 0, -EPROBE_DEFER, 0);
    check(-ENXIO, 0, -ENXIO, 0);
    check(55, -EBUSY, -EBUSY, 1);
    puts("MT8519 thermal lazy IRQ lookup and error propagation: PASS");
}
'''
    with tempfile.TemporaryDirectory(prefix="pinna-thermal-test-") as temporary:
        folder = Path(temporary)
        test = folder / "thermal.c"
        test.write_text(harness)
        executable = folder / "thermal-test"
        subprocess.run([os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Wextra",
                        "-Werror", str(test), "-o", str(executable)], check=True)
        subprocess.run([str(executable)], check=True)


if __name__ == "__main__":
    main()
