#pragma once
#include <sel4/config.h>
#undef CONFIG_LIB_SEL4_USE_THREAD_LOCALS
#include <sel4/sel4.h>
#include "platform.h"

enum { TOUCH_EP = 1, STORAGE_EP = 2, IPC_BUFFER = 0x200000, STACK_TOP = 0x140000 };
void touch_task(void);
void touch_map(void);
const char *touch_status(void);
bool touch_event(unsigned *x, unsigned *y, bool *down);
void clock_init(void);
void storage_task(void);
bool checkpoint(bool save,unsigned *tenths);

#ifdef VIOLET_COMPONENT
#define TOUCH_CLIENT_EP 2
#define STORAGE_CLIENT_EP 3
#else
#define TOUCH_CLIENT_EP TOUCH_EP
#define STORAGE_CLIENT_EP STORAGE_EP
#endif
