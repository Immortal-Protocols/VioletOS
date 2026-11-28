// How often the supervisor answers a fault in the Open Runtime with a warm
// restart (supervisor.c `open_warm_restart`, runtime/open/contain.rs) before
// it falls back to suspending the thread.
//
// A warm restart keeps the runtime's memory, so a fault that corrupted it, or
// a bug every pass of its loop hits, faults again at once. Three restarts
// are allowed in a row; the count starts over only after OPEN_WARM_QUIET_MS
// without a fault, and OPEN_WARM_TOTAL restarts per boot is the most there
// will ever be. A fast fault loop ends at the fourth fault, and faults paced
// out to dodge the quiet period end at the ninth: each restart keeps the
// same memory layout, so an attacker probing the runtime with crashes gets
// a handful of tries per boot, not an unlimited oracle. A rare fault (one
// bad call from one app now and then) is contained up to that cap.
//
// No kernel calls: system/supervisor/test_open_recover.c runs it on the host.
#pragma once

enum { OPEN_WARM_LIMIT = 3, OPEN_WARM_QUIET_MS = 10000, OPEN_WARM_TOTAL = 8 };

struct open_warm {
  unsigned long long last; // when the last fault came (ticks)
  unsigned count;          // warm restarts since the last quiet period
  unsigned total;          // warm restarts since the supervisor started
};

// A fault came at `now`; `quiet` is OPEN_WARM_QUIET_MS in ticks. Whether to
// restart the runtime's thread (1) or suspend it as before (0).
static inline int open_warm_allowed(struct open_warm *w, unsigned long long now,
                                    unsigned long long quiet) {
  if (now - w->last > quiet)
    w->count = 0;
  w->last = now;
  if (w->count >= OPEN_WARM_LIMIT || w->total >= OPEN_WARM_TOTAL)
    return 0;
  w->count++;
  w->total++;
  return 1;
}
