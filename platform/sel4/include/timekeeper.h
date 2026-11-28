// The timekeeper: one thread in the supervisor that owns the user timer and
// wakes every process that sleeps, so a delay blocks in the kernel instead of
// spinning on seL4_Yield (runtime/transport.c delay_ms) and the CPU reaches
// the kernel's idle thread when nothing is due.
//
// Each process shares one page with the timekeeper (TK_PAGE in the process)
// and holds two capabilities: its own notification to wait on (TK_WAIT_SLOT)
// and a send-only one, badged with the process's own bit, that makes the
// timekeeper take up its request (TK_KICK_SLOT). To sleep, a process writes
// how many counter ticks from now it wants its signal, kicks and waits. The
// timekeeper keeps the deadline itself (a process cannot change it except by
// kicking again, and cancels it by writing zero) and signals it once, so an
// early or stale wake only costs the sleeper another pass of its loop (it
// compares the clock itself), and a request that was made is always answered.
//
// Suspend-to-idle: Power (and only Power, by its page) may freeze the
// timekeeper. Frozen, it holds every other process's deadlines and releases
// them together once per `period`, so timer-driven work runs in one batch and
// the CPU idles in between. A request shorter than `hold_min` is not held:
// that is a process waiting on a device in the middle of work (a millisecond
// between polls), which must not take five hundred times as long, where the
// idle loops ask for longer. A process named in the wake mask ends the freeze
// by writing `wake`; Power ends it with a command.
//
// This file is the page layout and the decisions, with no kernel calls, so
// the host tests it (system/supervisor/tests/timekeeper_test.c, make
// test-unit). The kernel side is system/supervisor/timekeeper_thread.h.
#pragma once

enum {
  TK_PAGE = 0x00201000,     // the shared page, in every process
  TK_WAIT_SLOT = 29,        // the process's own notification
  TK_KICK_SLOT = 30,        // "take up my request" to the timekeeper
  TK_LIVE = 0x314b4954,     // "TIK1": passed to a process the timekeeper serves
  // A deadline's signal, where it is bound to a thread and arrives as a
  // receive's badge. Badges are 28 bits wide on this kernel.
  TK_BADGE = 1u << 27,
  // One kick bit each in the timekeeper's notification, below its interrupt's.
  TK_CLIENTS = 27,
  TK_IRQ_BADGE = 1u << 27,
  TK_OP_SUSPEND = 1,        // cmd's low byte; arg: the wake mask (one bit per client)
  TK_OP_RESUME = 2,         // arg: the reason
  TK_FROZEN = 1             // state bit 0
};

struct tk_page {
  // Written by the process.
  volatile unsigned ticks;  // a signal this many counter ticks after the kick; 0: none
  volatile unsigned wake;   // nonzero: end a freeze, with this reason
  volatile unsigned cmd;    // Power only: a count in the high bits, TK_OP_* in the low byte
  volatile unsigned arg;
  // Written by the timekeeper.
  volatile unsigned state;  // TK_FROZEN
  volatile unsigned reason; // why the last freeze ended
  volatile unsigned freezes;
  volatile unsigned held;   // deadlines held back by freezes so far
};

struct tk_client {
  struct tk_page *page;        // 0: no such client
  unsigned cmd;                // the cmd last carried out
  unsigned counted;            // this deadline was counted as held
  unsigned urgent;             // asked for less than hold_min: not held by a freeze
  unsigned long long deadline; // when to signal, in counter ticks; 0: nothing asked
};

struct timekeeper {
  struct tk_client c[TK_CLIENTS];
  unsigned power;  // the client whose commands are obeyed and who is never held; 0: none
  unsigned frozen;
  unsigned wake_mask;
  unsigned reason;
  unsigned freezes;
  unsigned held;
  unsigned long long thaw;   // when the held deadlines are next released
  unsigned long long period; // ticks between releases while frozen
  unsigned hold_min;         // requests shorter than this are served on time while frozen
};

// Look at every page at time `now`; `kicked` has a bit for each client that
// kicked since the last look. Returns the clients to signal, one bit each,
// and in `next` the time to look again (0: nothing is waiting).
static inline unsigned tk_scan(struct timekeeper *k, unsigned long long now, unsigned kicked,
                               unsigned long long *next) {
  unsigned fire = 0;
  unsigned long long soonest = 0;
  // Power's command first, so a wake written just after a suspend still counts.
  if (k->power && k->power < TK_CLIENTS && k->c[k->power].page) {
    struct tk_client *c = &k->c[k->power];
    unsigned cmd = c->page->cmd;
    if (cmd != c->cmd) {
      c->cmd = cmd;
      if ((cmd & 0xff) == TK_OP_SUSPEND && !k->frozen) {
        k->frozen = 1;
        k->wake_mask = c->page->arg;
        k->thaw = now + k->period;
        k->freezes++;
      } else if ((cmd & 0xff) == TK_OP_RESUME && k->frozen) {
        k->frozen = 0;
        k->reason = c->page->arg;
      }
    }
  }
  // A wake request is used up whether or not it counts: one made while
  // awake, or by a process the mask does not name, does not linger.
  for (unsigned i = 0; i < TK_CLIENTS; i++) {
    struct tk_page *p = k->c[i].page;
    if (!p) continue;
    unsigned wake = p->wake;
    if (!wake) continue;
    p->wake = 0;
    if (k->frozen && (k->wake_mask >> i & 1)) {
      k->frozen = 0;
      k->reason = wake;
    }
  }
  int release = !k->frozen;
  if (k->frozen && now >= k->thaw) {
    release = 1;
    k->thaw = now + (k->period ? k->period : 1);
  }
  for (unsigned i = 0; i < TK_CLIENTS; i++) {
    struct tk_client *c = &k->c[i];
    struct tk_page *p = c->page;
    if (!p) continue;
    p->state = k->frozen ? TK_FROZEN : 0;
    p->reason = k->reason;
    p->freezes = k->freezes;
    p->held = k->held;
    // A kick replaces the deadline with what the page asks for now, counted
    // from now. Zero in the page cancels, with or without a kick.
    unsigned ticks = p->ticks;
    if (kicked >> i & 1) {
      c->deadline = now + ticks;
      c->counted = 0;
      c->urgent = ticks < k->hold_min;
    }
    if (!ticks) c->deadline = 0;
    if (!c->deadline) continue;
    int exempt = i == k->power || c->urgent;
    if (c->deadline <= now && (release || exempt)) {
      fire |= 1u << i;
      c->deadline = 0;
      continue;
    }
    unsigned long long at = c->deadline;
    if (k->frozen && !exempt && at < k->thaw) {
      at = k->thaw;
      if (!c->counted) {
        c->counted = 1;
        k->held++;
      }
    }
    if (!soonest || at < soonest) soonest = at;
  }
  *next = soonest;
  return fire;
}

// What to write to the timer for a look at `next`: 0 stops it, otherwise the
// ticks from now (at least one, and within the 31 bits a down-counter takes).
static inline unsigned tk_timer_value(unsigned long long now, unsigned long long next) {
  if (!next) return 0;
  if (next <= now) return 1;
  unsigned long long d = next - now;
  return d > 0x7fffffffu ? 0x7fffffffu : (unsigned)d;
}

// A process's side: ask for a signal `ticks` from now (0: none). The caller
// kicks afterwards (not needed to cancel).
static inline void tk_page_arm(struct tk_page *p, unsigned ticks) {
  p->ticks = ticks;
}
// The ticks to ask for when the signal is wanted at `deadline`: what is left,
// at least one and at most what a request holds (the sleeper asks again).
static inline unsigned tk_ticks_until(unsigned long long now, unsigned long long deadline) {
  if (deadline <= now) return 1;
  return deadline - now > 0xffffffffu ? 0xffffffffu : (unsigned)(deadline - now);
}
static inline void tk_page_command(struct tk_page *p, unsigned op, unsigned arg) {
  p->arg = arg;
  p->cmd = ((p->cmd & ~0xffu) + 0x100) | (op & 0xff);
}

// The guard. The kernel lets every user thread program this timer
// (KernelArmExportPTMRUser), open apps included, so an app can stop it or
// move it and the timekeeper would never look again: every sleeper, and the
// supervisor's own health checks, would wait for good. The guard is a second
// thread at the priority apps and services share (so a spinning app cannot
// starve it: the kernel's time slice hands it a turn), which on each turn
// checks whether the timer is overdue and, if so, kicks the timekeeper
// itself (a kick with no client bits: a scan that fires what is due and
// programs the timer again). Tampering then costs every sleeper at most a
// time slice, never a hang. With nothing else to run, the guard waits for an
// interrupt (WFI), so the CPU still idles; the kernel's own tick and every
// device interrupt end the wait.
struct tk_watch {
  volatile unsigned seq;    // odd while the timekeeper writes `due`
  volatile unsigned lo, hi; // when the timer should fire, in counter ticks; 0: stopped
  volatile unsigned late;   // turns that found it overdue and kicked
  volatile unsigned wfi;    // 0 not yet, 1 in its first WFI, 2 WFI works, 3 no WFI: yields only
};
// The timekeeper, after programming the timer with `value` (tk_timer_value) at `now`.
static inline void tk_watch_set(struct tk_watch *w, unsigned long long now, unsigned value) {
  unsigned long long due = value ? now + value : 0;
  w->seq++;
  __sync_synchronize();
  w->lo = (unsigned)due;
  w->hi = (unsigned)(due >> 32);
  __sync_synchronize();
  w->seq++;
}
// The guard's read: 0 if the timekeeper was writing (the next turn reads again).
static inline int tk_watch_get(const struct tk_watch *w, unsigned long long *due) {
  unsigned seq = w->seq;
  __sync_synchronize();
  unsigned long long at = (unsigned long long)w->hi << 32 | w->lo;
  __sync_synchronize();
  if ((seq & 1) || seq != w->seq) return 0;
  *due = at;
  return 1;
}
// One turn of the guard at `now`: 1 to kick, when the timer is more than
// `slack` ticks past when it should have fired (the timekeeper, at the top
// priority, takes a working timer's interrupt at once).
static inline int tk_guard_turn(struct tk_watch *w, unsigned long long now, unsigned long long slack) {
  unsigned long long due;
  if (!tk_watch_get(w, &due) || !due || now <= due || now - due <= slack) return 0;
  w->late++;
  return 1;
}
// After its yield (from `before` to `after`): whether the guard may wait for
// an interrupt. Only if nobody else ran (the yield came straight back) and
// WFI has not been found to trap.
static inline int tk_guard_may_wait(const struct tk_watch *w, unsigned long long before,
                                    unsigned long long after, unsigned quick) {
  return w->wfi != 3 && after - before < quick;
}
// The supervisor's look at the guard, once per health check: 1 when the
// guard has sat in its first WFI for `checks` looks in a row. Every
// interrupt (the kernel's tick, the health check's own deadline) ends a WFI,
// so that means the instruction trapped at user level (SCTLR.nTWI clear) and
// the thread died there. The supervisor then restarts it yielding only:
// the guard is kept at the cost of the idle CPU.
static inline int tk_guard_stuck(const struct tk_watch *w, unsigned *looks, unsigned checks) {
  if (w->wfi != 1) {
    *looks = 0;
    return 0;
  }
  return ++*looks >= checks;
}
// Whether the timekeeper serves process `id` (of `total`): not one in `skip`
// (one bit per process), and not past the clients it has pages for.
static inline int tk_serves(unsigned id, unsigned total, unsigned skip) {
  return id && id < total && id < TK_CLIENTS && !(skip >> id & 1);
}
