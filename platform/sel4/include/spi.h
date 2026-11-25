#pragma once
#include "platform.h"
bool spi_clock_start(void);
unsigned spi_read(unsigned offset);
void spi_write(unsigned offset,unsigned value);
/* The touch controller's interrupt line (the handset's EINT 10; the QEMU
   fixture has none). Start programs it for falling edges and reports
   whether there is one; take acknowledges a latched edge, if any. */
bool touch_irq_start(void);
bool touch_irq_take(void);
void touch_irq_stop(void);
