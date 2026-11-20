#pragma once
#include <stdint.h>
#include <stdbool.h>
void *map_device(uint32_t physical, uint32_t virt, unsigned bits);
uint32_t ticks(void);
extern uint32_t ticks_per_ms;
void delay_ms(unsigned ms);
void stopwatch(void);
bool touch_sample(unsigned *x, unsigned *y, bool *down);
void touch_initialize(char status[40]);
void watchdog_attach(volatile uint32_t *registers);
void watchdog_progress(void);
