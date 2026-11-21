// MT6765 TOPRGU, from Motorola's drivers/watchdog/mtk_wdt.c.
#include "platform.h"
static volatile uint32_t *reload;
static uint32_t last_reload;

void watchdog_attach(volatile uint32_t *registers) {
    if (registers[0] & 1u) {
        reload = &registers[0x08 / sizeof(uint32_t)];
        *reload = 0x1971;
        last_reload = ticks();
    }
}

void watchdog_progress(void) {
    if (!reload) return;
    uint32_t now = ticks();
    if (now - last_reload >= 100 * ticks_per_ms) {
        *reload = 0x1971;
        last_reload = now;
    }
}
