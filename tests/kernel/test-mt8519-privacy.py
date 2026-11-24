#!/usr/bin/env python3
"""Compile the privacy event and latch helpers with GPIO/input mocks.

Run after applying the kernel patches:
    python3 tests/kernel/test-mt8519-privacy.py [path/to/linux] [path/to/gpio-privacy.c]
The MT8519 device-tree contract is read from the kernel source tree. Requires cc.
"""
from pathlib import Path
import os
import re
import subprocess
import sys
import tempfile


def section(text, start, end):
    return text[text.index(start):text.index(end, text.index(start))]


def check_dt_contract(path):
    text = path.read_text()
    root = section(text, "gpio_privacy: gpio-privacy {", "\n\t};")
    state = section(root, "state_event {", "\n\t\t};")
    button = section(root, "button_event {", "\n\t\t};")
    assert 'compatible = "gpio-privacy"' in root
    assert re.search(r'enable-gpio-toggle-duration\s*=\s*<50>', root)
    assert re.search(r'linux,input-type\s*=\s*<EV_SW>', state)
    assert re.search(r'linux,code\s*=\s*<0x0e>', state)
    assert re.search(r'gpios\s*=\s*<[^;]+MUTE_STATE_L[^;]+>', state)
    assert re.search(r'linux,input-type\s*=\s*<EV_KEY>', button)
    assert re.search(r'linux,code\s*=\s*<KEY_MUTE>', button)
    assert re.search(r'debounce-interval\s*=\s*<50>', button)
    assert re.search(r'gpios\s*=\s*<[^;]+BTN_MUTE_L[^;]+>', button)


def main():
    source_root = Path(sys.argv[1] if len(sys.argv) > 1 else "build/linux").resolve()
    driver = (Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else
              source_root / "drivers/input/misc/gpio-privacy.c")
    text = driver.read_text()
    check_dt_contract(source_root / "arch/arm/boot/dts/mediatek/cannoli-cheesecake.dtsi")

    helpers = section(text, "static int gpio_privacy_read(", "static ssize_t state_show(")
    helpers += section(text, "static void gpio_privacy_pulse_enable(", "static ssize_t enable_store(")
    helpers += section(text, "static void gpio_privacy_state_event(", "static void gpio_privacy_child_work(")
    helpers += section(text, "static void gpio_privacy_child_work(", "static irqreturn_t gpio_privacy_irq(")
    helpers += section(text, "static void gpio_privacy_cancel_work(", "static int gpio_privacy_setup_event(")
    helpers += section(text, "static int gpio_privacy_request_event_irq(",
                       "static int gpio_privacy_probe_child_events(")

    harness = r'''
#include <assert.h>
#include <errno.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#define GPIO_PRIVACY_LEGACY_WAIT_MS 100
#define container_of(ptr, type, member) ((type *)((char *)(ptr) - offsetof(type, member)))
#define to_delayed_work(work) container_of(work, struct delayed_work, work)
struct gpio_desc { int physical; bool active_low; int error; int writes[8]; int nwrites; };
struct gpio_descs { int ndescs; struct gpio_desc **desc; };
struct work_struct { int unused; };
struct delayed_work { struct work_struct work; int id; };
struct input_dev { int events[8][3]; int count; int syncs; };
struct gpio_privacy_event {
    struct gpio_privacy *privacy;
    struct gpio_desc *gpio;
    struct input_dev *input;
    unsigned int type, code, debounce_ms;
    int last_value;
    struct delayed_work work;
};
struct device;
struct gpio_privacy {
    struct device *dev;
    struct gpio_descs *state;
    struct gpio_desc *enable;
    int lock;
    struct input_dev *input;
    unsigned int code, debounce_ms, enable_pulse_ms;
    bool child_events;
    struct gpio_privacy_event state_event, button_event;
    struct delayed_work work;
};
static int slept_ms, canceled[3], cancel_count;
static int gpiod_get_value_cansleep(struct gpio_desc *gpio)
{
    if (gpio->error) return gpio->error;
    return gpio->active_low ? !gpio->physical : gpio->physical;
}
static void gpiod_set_value_cansleep(struct gpio_desc *gpio, int value)
{
    gpio->writes[gpio->nwrites++] = value;
}
static void input_event(struct input_dev *input, unsigned int type,
                        unsigned int code, int value)
{
    input->events[input->count][0] = type;
    input->events[input->count][1] = code;
    input->events[input->count++][2] = value;
}
static void input_sync(struct input_dev *input) { ++input->syncs; }
static void mutex_lock(int *lock) { (void)lock; }
static void mutex_unlock(int *lock) { (void)lock; }
static void usleep_range(unsigned int min, unsigned int max)
{ (void)min; (void)max; }
static void msleep(unsigned int ms) { slept_ms += ms; }
static void cancel_delayed_work_sync(struct delayed_work *work)
{ canceled[cancel_count++] = work->id; }
#define EV_KEY 1
#define EV_SW 5
#define KEY_MUTE 113
/* The Cannoli's privacy lines are on the I2C CPLD, whose IRQs are nested
 * threaded. On those the kernel refuses a hard handler with -EINVAL
 * (__setup_irq without thread_fn) and request_any_context_irq returns
 * IRQC_IS_NESTED. A C2N6L4 failed the probe with -22 that way. */
#define IRQF_TRIGGER_RISING 1
#define IRQF_TRIGGER_FALLING 2
#define IRQC_IS_NESTED 1
typedef int irqreturn_t;
struct device { int unused; };
static int requested_irq, requested_flags;
static int gpiod_to_irq(struct gpio_desc *gpio) { return gpio->error ? gpio->error : 168; }
static int dev_err_probe(struct device *dev, int err, const char *fmt, ...)
{ (void)dev; (void)fmt; return err; }
static int devm_request_irq(struct device *dev, int irq,
                            irqreturn_t (*handler)(int, void *), unsigned long flags,
                            const char *name, void *data)
{ (void)dev; (void)irq; (void)handler; (void)flags; (void)name; (void)data; return -EINVAL; }
static int devm_request_any_context_irq(struct device *dev, int irq,
                                        irqreturn_t (*handler)(int, void *), unsigned long flags,
                                        const char *name, void *data)
{ (void)dev; (void)handler; (void)name; (void)data; requested_irq = irq;
  requested_flags = flags; return IRQC_IS_NESTED; }
static irqreturn_t gpio_privacy_event_irq(int irq, void *data) { (void)irq; (void)data; return 0; }
'''
    harness += helpers
    harness += r'''
int main(void)
{
    struct gpio_desc state_gpio = { .physical = 0, .active_low = true };
    struct gpio_desc button_gpio = { .physical = 0, .active_low = true };
    struct gpio_desc enable_gpio = {0};
    struct gpio_privacy privacy = {
        .enable = &enable_gpio, .child_events = true, .enable_pulse_ms = 50,
        .state_event = { .gpio = &state_gpio },
        .button_event = { .gpio = &button_gpio },
    };
    struct input_dev state_input = {0}, button_input = {0};
    privacy.state_event.input = &state_input;
    privacy.state_event.type = EV_SW;
    privacy.state_event.code = 0x0e;
    privacy.button_event.input = &button_input;
    privacy.button_event.type = EV_KEY;
    privacy.button_event.code = KEY_MUTE;

    gpio_privacy_child_work(&privacy.state_event.work.work);
    assert(state_input.count == 1 && state_input.events[0][0] == EV_SW);
    assert(state_input.events[0][1] == 0x0e && state_input.events[0][2] == 1);
    assert(state_input.syncs == 1);

    button_gpio.physical = 1; /* quick press and release before debounce: sample is still released */
    gpio_privacy_child_work(&privacy.button_event.work.work);
    assert(button_input.count == 2);
    assert(button_input.events[0][0] == EV_KEY && button_input.events[0][1] == KEY_MUTE);
    assert(button_input.events[0][2] == 1 && button_input.events[1][2] == 0);
    button_gpio.physical = 0;
    gpio_privacy_child_work(&privacy.button_event.work.work);
    button_gpio.physical = 1;
    gpio_privacy_child_work(&privacy.button_event.work.work);
    assert(button_input.count == 4);
    assert(button_input.events[2][2] == 1 && button_input.events[3][2] == 0);
    assert(button_input.syncs == 4);

    assert(gpio_privacy_enable(&privacy, 1) == 0);
    assert(slept_ms == 50 && enable_gpio.nwrites == 2);
    assert(enable_gpio.writes[0] == 1 && enable_gpio.writes[1] == 0);
    assert(gpio_privacy_enable(&privacy, 0) == -EINVAL);
    assert(gpio_privacy_enable(&privacy, -1) == -EINVAL);
    assert(enable_gpio.nwrites == 2);

    state_gpio.physical = 1; /* valid logical state 0 is a timeout */
    assert(gpio_privacy_enable(&privacy, 1) == -ETIMEDOUT);
    assert(slept_ms == 100);
    state_gpio.error = -5;
    button_gpio.error = -5;
    state_input.count = button_input.count = 0;
    gpio_privacy_child_work(&privacy.state_event.work.work);
    gpio_privacy_child_work(&privacy.button_event.work.work);
    assert(state_input.count == 0 && button_input.count == 0);
    assert(gpio_privacy_read(&privacy) == -5);
    assert(gpio_privacy_enable(&privacy, 1) == -5);
    assert(slept_ms == 150);

    privacy.work.id = 1;
    privacy.state_event.work.id = 2;
    privacy.button_event.work.id = 3;
    gpio_privacy_cancel_work(&privacy);
    assert(cancel_count == 3 && canceled[0] == 1 && canceled[1] == 2 && canceled[2] == 3);

    struct device dev = {0};
    state_gpio.error = 0;
    assert(gpio_privacy_request_event_irq(&(struct gpio_privacy){ .dev = &dev },
                                          &privacy.state_event, "gpio-privacy-state") == 0);
    assert(requested_irq == 168);
    assert(requested_flags == (IRQF_TRIGGER_RISING | IRQF_TRIGGER_FALLING));
    (void)devm_request_irq;
    (void)devm_request_any_context_irq;
    puts("MT8519 privacy switch, button, pulse, error and cleanup helpers: PASS");
    return 0;
}
'''

    # Enforce the managed teardown order: IRQ release actions are registered
    # after cancellation, and managed input devices are allocated beforehand.
    probe = section(text, "static int gpio_privacy_probe(", "static const struct of_device_id")
    assert probe.index("gpio_privacy_probe_child_events") < probe.index("devm_add_action_or_reset")
    assert probe.index("gpio_privacy_probe_legacy") < probe.index("devm_add_action_or_reset")
    assert probe.index("devm_add_action_or_reset") < probe.index("gpio_privacy_request_event_irq")
    assert probe.index("devm_add_action_or_reset") < probe.index("gpio_privacy_request_legacy_irqs")

    with tempfile.TemporaryDirectory(prefix="pinna-mt8519-privacy-") as tmp:
        test = Path(tmp) / "privacy-test.c"
        executable = Path(tmp) / "privacy-test"
        test.write_text(harness)
        subprocess.run([os.environ.get("CC", "cc"), "-std=gnu11", "-Wall", "-Wextra",
                        "-Werror", "-Wno-unused-parameter", str(test), "-o", str(executable)],
                       check=True)
        subprocess.run([str(executable)], check=True)


if __name__ == "__main__":
    main()
