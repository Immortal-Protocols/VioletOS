#pragma once
#include "platform.h"
bool spi_clock_start(void);
unsigned spi_read(unsigned offset);
void spi_write(unsigned offset,unsigned value);
unsigned spi_timeout_command(void);
unsigned spi_timeout_state(void);
bool exchange(uint8_t *data,unsigned len);
bool bus_start(void);
