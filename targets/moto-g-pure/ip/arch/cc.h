#pragma once
#include <stdint.h>
unsigned radio_ip_random(void);
#define LWIP_RAND() radio_ip_random()
#define LWIP_PLATFORM_DIAG(x) ((void)0)
#define LWIP_PLATFORM_ASSERT(x) __builtin_trap()
