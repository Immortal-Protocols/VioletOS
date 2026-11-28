#pragma once
#include <stdint.h>
#ifdef __aarch64__
// The NDK's ssize_t is long here, not the int lwIP would define without SSIZE_MAX.
#include <sys/types.h>
#include <limits.h>
#ifndef SSIZE_MAX
#define SSIZE_MAX LONG_MAX
#endif
#define LWIP_NO_UNISTD_H 1
#endif
unsigned radio_ip_random(void);
#define LWIP_RAND() radio_ip_random()
#define LWIP_PLATFORM_DIAG(x) ((void)0)
#define LWIP_PLATFORM_ASSERT(x) __builtin_trap()
