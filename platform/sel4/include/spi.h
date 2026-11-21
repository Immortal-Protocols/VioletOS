#pragma once
#include "platform.h"
bool spi_clock_start(void);
unsigned spi_read(unsigned offset);
void spi_write(unsigned offset,unsigned value);
