#ifndef VIOLET_POWER_H
#define VIOLET_POWER_H

#include <stdbool.h>

/* Proposed UI-facing service contract. No backend is linked yet. */
struct violet_battery_status {
    int percent; /* 0..100, or -1 if the gauge cannot supply a level. */
    bool external_power;
};

/* Read cached state without blocking on hardware. Return 0 on success or
 * negative errno on failure. On failure, leave the caller's output untouched.
 */
int violet_battery_read(struct violet_battery_status *out);

/* Queue a suspend request. A power manager may reject it during an active
 * call. Success means accepted; the eventual sleep/wake cycle is asynchronous.
 */
int violet_suspend_request(void);

#endif
