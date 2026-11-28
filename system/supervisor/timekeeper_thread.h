// The timekeeper's kernel side, included by supervisor.c. The decisions are
// in platform/sel4/include/timekeeper.h (host-tested); this file gives them a
// thread, the user timer and each process's page and notifications.
//
// The thread shares the supervisor's address space but has no IPC buffer:
// libsel4 keeps one buffer pointer per program, so everything it calls here
// passes its arguments in registers only (tk_wait, tk_signal, tk_ack).
//
// The timer is the nonsecure physical timer (PPI 30), the one user timer
// both boards have. While the timekeeper runs it serves the Open Runtime's
// deadlines too (runtime/open/sel4.c). If the timer does not fire at start,
// the timekeeper stays off: every process is started without TK_LIVE and
// keeps its yield loop, and the Open Runtime gets the timer as before.
//
// VIOLET_TIMEKEEPER=0 at build time turns it off the same way.
//
// The kernel exports this timer to every user thread, open apps included,
// so an app can stop or move it. A guard thread (timekeeper.h, tk_watch)
// notices when the timer is overdue and kicks the timekeeper, so that costs
// sleepers a time slice, not a hang. The timekeeper runs only with its
// guard. What the guard cannot undo: an app that keeps re-arming the timer
// to fire at once makes the timekeeper scan more often, within the app's own
// share of the CPU. The fix that removes the exposure is in the kernel
// (export the timer to the supervisor's threads only); see services/power/README.md.
#pragma once
#include "timekeeper.h"

#ifndef VIOLET_TIMEKEEPER
#define VIOLET_TIMEKEEPER 1
#endif

enum {
  // The clients' pages in the supervisor, in the section the lane pages use.
  TK_ROOT_VA = LANE_BASE + 0x80000,
  // Root CNode slots left alone for whatever is allocated after start-up.
  TK_SLOT_RESERVE = 192,
  // The supervisor's own look at its processes' health.
  TK_ROOT_MS = 100,
  // Timer-driven work runs this often while suspended to idle,
  TK_FREEZE_MS = 500,
  // except waits shorter than this: a process polling a device mid-work.
  TK_HOLD_MIN_MS = 3,
  // The guard (timekeeper.h): the priority every process and app thread
  // runs at without MCS, how late the timer may be before it kicks, and
  // health checks in a row stuck in its first WFI before it yields instead.
  TK_GUARD_PRIORITY = 100,
  TK_GUARD_SLACK_MS = 2,
  TK_GUARD_STUCK_CHECKS = 3
};

static struct timekeeper tk;
static struct tk_page tk_root_page;
static struct tk_watch tk_watch;
static seL4_CPtr tk_ntfn, tk_irq, tk_thread, tk_root_ntfn, tk_root_kick, tk_notify[TK_CLIENTS];
static seL4_CPtr tk_guard, tk_guard_kick;
static unsigned tk_per_ms, tk_guard_looks;
static volatile unsigned tk_ready;
static char tk_stack[4096] __attribute__((aligned(16)));
static char tk_guard_stack[1024] __attribute__((aligned(16)));

static unsigned long long tk_now(void) {
  unsigned lo, hi;
  __asm__ volatile("isb; mrrc p15,1,%0,%1,c14" : "=r"(lo), "=r"(hi));
  return ((unsigned long long)hi << 32) | lo;
}
static unsigned tk_ticks_per_ms(void) {
  unsigned freq;
  __asm__ volatile("mrc p15,0,%0,c14,c0,0" : "=r"(freq));
  return freq / 1000;
}
// Register-only system calls for the timekeeper thread.
static seL4_Word tk_wait(seL4_CPtr notification) {
  seL4_Word badge = 0, info, m0, m1, m2, m3;
#ifdef CONFIG_KERNEL_MCS
  arm_sys_recv(seL4_SysWait, notification, &badge, &info, &m0, &m1, &m2, &m3, 0);
#else
  arm_sys_recv(seL4_SysRecv, notification, &badge, &info, &m0, &m1, &m2, &m3, 0);
#endif
  return badge;
}
// Non-blocking, so a capability deleted under it (a rebuild) is no fault.
static void tk_signal(seL4_CPtr notification) {
  arm_sys_send_null(seL4_SysNBSend, notification, seL4_MessageInfo_new(0, 0, 0, 0).words[0]);
}
static void tk_ack(seL4_CPtr handler) {
  seL4_CallWithMRs(handler, seL4_MessageInfo_new(IRQAckIRQ, 0, 0, 0), 0, 0, 0, 0);
}
static void tk_program(unsigned ticks) {
  if (ticks) __asm__ volatile("mcr p15, 0, %0, c14, c2, 0" ::"r"(ticks)); // CNTP_TVAL
  __asm__ volatile("mcr p15, 0, %0, c14, c2, 1" ::"r"(ticks ? 1u : 0u)); // CNTP_CTL
  __asm__ volatile("isb");
}
static void tk_main(void) {
  for (;;) {
    seL4_Word badge = tk_wait(tk_ntfn);
    unsigned long long now = tk_now(), next = 0;
    unsigned fire = tk_scan(&tk, now, badge & ((1u << TK_CLIENTS) - 1), &next);
    for (unsigned i = 0; fire; i++, fire >>= 1)
      if ((fire & 1) && tk_notify[i]) tk_signal(tk_notify[i]);
    // The timer is level-triggered: move it on before taking the interrupt again.
    unsigned value = tk_timer_value(now, next);
    tk_program(value);
    tk_watch_set(&tk_watch, now, value);
    if (badge & TK_IRQ_BADGE) tk_ack(tk_irq);
  }
}

// The guard (timekeeper.h): no IPC buffer either, so register-only calls.
// A kick from it has badge 0: no client's request is re-read, the scan
// only fires what is due and programs the timer again.
static void tk_guard_main(void) {
  unsigned long long slack = (unsigned long long)TK_GUARD_SLACK_MS * tk_per_ms;
  // A yield that comes back within 50 microseconds ran nobody else.
  unsigned quick = tk_per_ms / 20 ? tk_per_ms / 20 : 1;
  for (;;) {
    if (tk_guard_turn(&tk_watch, tk_now(), slack)) tk_signal(tk_guard_kick);
    unsigned long long before = tk_now();
    seL4_Yield();
    if (tk_guard_may_wait(&tk_watch, before, tk_now(), quick)) {
      if (!tk_watch.wfi) tk_watch.wfi = 1;
      __asm__ volatile("dsb; wfi" ::: "memory");
      if (tk_watch.wfi == 1) tk_watch.wfi = 2;
    }
  }
}
// (Re)start the guard at its entry.
static bool tk_guard_run(void) {
  seL4_UserContext regs = {.pc = (seL4_Word)tk_guard_main,
                           .sp = (seL4_Word)(tk_guard_stack + sizeof(tk_guard_stack))};
  return !seL4_TCB_WriteRegisters(tk_guard, 1, 0, sizeof(regs) / sizeof(seL4_Word), &regs);
}
static seL4_CPtr tk_make(unsigned type);
static seL4_CPtr tk_mint(seL4_CPtr source, seL4_CapRights_t rights, unsigned badge);
// The guard's thread, at the priority apps run at. False: no guard, and so
// no timekeeper (a timer any app can stop must not be the one every
// process waits on).
static bool tk_guard_start(void) {
  seL4_CPtr tcb = tk_make(seL4_TCBObject);
  tk_guard_kick = tk_ntfn ? tk_mint(tk_ntfn, seL4_AllRights, 0) : 0;
  if (!tcb || !tk_guard_kick) return false;
#ifdef CONFIG_KERNEL_MCS
  if (seL4_TCB_Configure(tcb, seL4_CapInitThreadCNode, 0, seL4_CapInitThreadVSpace, 0, 0, 0)) return false;
  give_time(tcb, TK_GUARD_PRIORITY, 0);
#else
  if (seL4_TCB_Configure(tcb, 0, seL4_CapInitThreadCNode, 0, seL4_CapInitThreadVSpace, 0, 0, 0) ||
      seL4_TCB_SetPriority(tcb, seL4_CapInitThreadTCB, TK_GUARD_PRIORITY))
    return false;
#endif
  tk_guard = tcb;
  if (tk_guard_run()) return true;
  seL4_TCB_Suspend(tcb);
  tk_guard = 0;
  return false;
}

// An object from the shared untyped, or 0: the timekeeper is an extra, and
// never stops a boot that is short of memory or capability slots.
static seL4_CPtr tk_make(unsigned type) {
  if (boot->empty.end - next_slot <= TK_SLOT_RESERVE) return 0;
  if (seL4_Untyped_Retype(ram, type, 0, seL4_CapInitThreadCNode, 0, 0, next_slot, 1)) return 0;
  return next_slot++;
}
static seL4_CPtr tk_mint(seL4_CPtr source, seL4_CapRights_t rights, unsigned badge) {
  if (boot->empty.end - next_slot <= TK_SLOT_RESERVE) return 0;
  if (seL4_CNode_Mint(seL4_CapInitThreadCNode, next_slot, 32, seL4_CapInitThreadCNode, source, 32, rights, badge))
    return 0;
  return next_slot++;
}
// The supervisor is client 0.
static void tk_arm_root(unsigned ticks) {
  tk_page_arm(&tk_root_page, ticks);
  seL4_Signal(tk_root_kick);
}

// Give process `id` its page, its notification and the kick. `notify`: the
// capability to signal its deadlines on, or 0 for a notification of its own
// at TK_WAIT_SLOT. All or nothing: a process is told TK_LIVE only if this
// returned true.
static bool tk_attach(unsigned id, seL4_CPtr notify) {
  struct task *t = &procs[id].task;
  if (!tk_ready || id >= TK_CLIENTS || !t->pd || !t->caps || tk.c[id].page) return false;
  unsigned bits = t->cap_bits ? t->cap_bits : CAP_BITS;
  seL4_CPtr frame = tk_make(seL4_ARM_SmallPageObject);
  if (!frame || boot->empty.end - next_slot <= TK_SLOT_RESERVE) return false;
  seL4_CPtr copy = next_slot;
  if (seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, 32, seL4_CapInitThreadCNode, frame, 32, seL4_AllRights))
    return false;
  next_slot++;
  // The process's IPC buffer shares this section, so its page table exists.
  if (seL4_ARM_Page_Map(frame, t->pd, TK_PAGE, seL4_AllRights,
                        seL4_ARM_Default_VMAttributes | seL4_ARM_ExecuteNever))
    return false;
  unsigned va = TK_ROOT_VA + id * PAGE;
  ensure_table(&bootstrap, va);
  if (seL4_ARM_Page_Map(copy, bootstrap.pd, va, seL4_AllRights,
                        seL4_ARM_Default_VMAttributes | seL4_ARM_ExecuteNever))
    return false;
  seL4_CPtr own = 0;
  if (!notify) {
    own = tk_make(seL4_NotificationObject);
    notify = own ? tk_mint(own, seL4_AllRights, TK_BADGE) : 0;
    if (!notify) return false;
    if (seL4_CNode_Mint(t->caps, TK_WAIT_SLOT, bits, seL4_CapInitThreadCNode, own, 32,
                        seL4_CapRights_new(0, 0, 1, 0), 0))
      return false;
    // Power's deadlines arrive between its requests (services/power/process.rs).
    if (id == POWER && seL4_TCB_BindNotification(t->thread, own)) return false;
  }
  if (seL4_CNode_Mint(t->caps, TK_KICK_SLOT, bits, seL4_CapInitThreadCNode, tk_ntfn, 32,
                      seL4_CapRights_new(0, 0, 0, 1), 1u << id))
    return false;
  tk_notify[id] = notify;
  tk.c[id].page = (struct tk_page *)va;
  if (id == POWER) tk.power = POWER;
  return true;
}

// Before the Open Runtime is wired: the thread, the timer and a check that
// it fires.
static void timekeeper_start(void) {
  memset(&tk, 0, sizeof(tk));
  memset(&tk_root_page, 0, sizeof(tk_root_page));
  memset(tk_notify, 0, sizeof(tk_notify));
  memset(&tk_watch, 0, sizeof(tk_watch));
  tk_ready = 0;
  tk_ntfn = tk_irq = tk_thread = tk_root_ntfn = tk_root_kick = tk_guard = tk_guard_kick = 0;
  tk_guard_looks = 0;
#if VIOLET_TIMEKEEPER && !defined(VIOLET_TEST_ISOLATION)
  tk_per_ms = tk_ticks_per_ms();
  tk.period = (unsigned long long)TK_FREEZE_MS * tk_ticks_per_ms();
  tk.hold_min = TK_HOLD_MIN_MS * tk_ticks_per_ms();
  tk_ntfn = tk_make(seL4_NotificationObject);
  tk_root_ntfn = tk_make(seL4_NotificationObject);
  seL4_CPtr tcb = tk_make(seL4_TCBObject);
  seL4_CPtr irq_badged = tk_ntfn ? tk_mint(tk_ntfn, seL4_AllRights, TK_IRQ_BADGE) : 0;
  seL4_CPtr root_badged = tk_root_ntfn ? tk_mint(tk_root_ntfn, seL4_AllRights, TK_BADGE) : 0;
  tk_root_kick = tk_ntfn ? tk_mint(tk_ntfn, seL4_AllRights, 1u << 0) : 0;
  if (!tcb || !irq_badged || !root_badged || !tk_root_kick || boot->empty.end - next_slot <= TK_SLOT_RESERVE) return;
  seL4_CPtr handler = next_slot++;
  if (seL4_IRQControl_Get(seL4_CapIRQControl, OPEN_TIMER_IRQ, seL4_CapInitThreadCNode, handler, 32)) return;
  tk_irq = handler;
  tk_program(0);
  bool ok = !seL4_IRQHandler_SetNotification(handler, irq_badged);
#ifdef CONFIG_KERNEL_MCS
  ok = ok && !seL4_TCB_Configure(tcb, seL4_CapInitThreadCNode, 0, seL4_CapInitThreadVSpace, 0, 0, 0);
  if (ok) give_time(tcb, seL4_MaxPrio, 0);
#else
  ok = ok && !seL4_TCB_Configure(tcb, 0, seL4_CapInitThreadCNode, 0, seL4_CapInitThreadVSpace, 0, 0, 0) &&
       !seL4_TCB_SetPriority(tcb, seL4_CapInitThreadTCB, seL4_MaxPrio);
#endif
  seL4_UserContext regs = {.pc = (seL4_Word)tk_main, .sp = (seL4_Word)(tk_stack + sizeof(tk_stack))};
  tk.c[0].page = &tk_root_page;
  tk_notify[0] = root_badged;
  ok = ok && !seL4_TCB_WriteRegisters(tcb, 1, 0, sizeof(regs) / sizeof(seL4_Word), &regs);
  if (ok) {
    tk_thread = tcb;
    // A millisecond's deadline must be signalled within fifty.
    unsigned long long start = tk_now(), limit = 50ull * tk_ticks_per_ms();
    tk_arm_root(tk_ticks_per_ms());
    ok = false;
    while (!ok && tk_now() - start < limit) {
      seL4_Word badge = 0;
      seL4_Poll(tk_root_ntfn, &badge);
      ok = badge != 0;
      // At start-up the supervisor runs at the timekeeper's own priority.
      if (!ok) seL4_Yield();
    }
  }
  // It runs only once the supervisor drops to its priority (TK_GUARD_PRIORITY).
  ok = ok && tk_guard_start();
  if (!ok) {
    // Off: stop the thread and free the interrupt for the Open Runtime.
    seL4_TCB_Suspend(tcb);
    tk_program(0);
    seL4_CNode_Delete(seL4_CapInitThreadCNode, handler, 32);
    tk_irq = tk_thread = 0;
    tk.c[0].page = 0;
    return;
  }
  // A deadline wakes the supervisor's receive on its lifecycle endpoint.
  seL4_TCB_UnbindNotification(seL4_CapInitThreadTCB);
  if (seL4_TCB_BindNotification(seL4_CapInitThreadTCB, tk_root_ntfn)) tk_root_ntfn = 0;
  tk_ready = 1;
#endif
}

// The Open Runtime as a client: its deadlines are signalled on its own wake
// notification with the timer's badge (`badged`), and the kick is also where
// it expects its timer, so its start-up check finds both. False: the
// timekeeper is off and the Open Runtime takes the interrupt itself.
static void timekeeper_stop(void);
static bool timekeeper_open(struct task *t, seL4_CPtr wake, seL4_CPtr badged) {
  if (!tk_ready) return false;
  if (t->cap_bits != OPEN_BITS || !tk_attach(OPEN_RT, badged)) {
    // All or nothing: the timekeeper holds the timer's interrupt, so the
    // Open Runtime could not take it. Stop the timekeeper before anyone else
    // is attached: every process keeps its yield loop and the Open Runtime
    // gets the timer, as with VIOLET_TIMEKEEPER=0.
    timekeeper_stop();
    return false;
  }
  seL4_CapRights_t send = seL4_CapRights_new(0, 0, 0, 1);
  check(seL4_CNode_Mint(t->caps, OPEN_SLOT_TIMER, OPEN_BITS, seL4_CapInitThreadCNode, tk_ntfn, 32, send, 1u << OPEN_RT));
  check(seL4_CNode_Copy(t->caps, OPEN_SLOT_WAKE, OPEN_BITS, seL4_CapInitThreadCNode, wake, 32, seL4_AllRights));
  check(seL4_CNode_Copy(t->caps, TK_WAIT_SLOT, OPEN_BITS, seL4_CapInitThreadCNode, wake, 32, seL4_AllRights));
  check(seL4_TCB_BindNotification(t->thread, wake));
  return true;
}

// After everything else is wired: every other process that sleeps. Not the
// Open Runtime (timekeeper_open), and not the Sentinel, which never sleeps
// and is meant to hold one endpoint and its two windows only.
static void timekeeper_attach_all(void) {
  for (unsigned id = 1; id < TOTAL && tk_ready; id++)
    if (tk_serves(id, TOTAL, 1u << OPEN_RT | 1u << SENTINEL)) tk_attach(id, 0);
}

// What a process is started with in r1: TK_LIVE if it may sleep on the timekeeper.
static unsigned timekeeper_token(unsigned id) {
  return tk_ready && id < TK_CLIENTS && tk.c[id].page ? TK_LIVE : 0;
}

// Before the system is taken apart: the thread stops and the interrupt is free.
static void timekeeper_stop(void) {
  if (tk_thread) seL4_TCB_Suspend(tk_thread);
  if (tk_guard) seL4_TCB_Suspend(tk_guard);
  tk_guard = 0;
  tk_ready = 0;
  if (tk_irq) {
    tk_program(0);
    seL4_CNode_Delete(seL4_CapInitThreadCNode, tk_irq, 32);
  }
  seL4_TCB_UnbindNotification(seL4_CapInitThreadTCB);
  tk_irq = tk_thread = 0;
}

// The supervisor's wait for a fault or a lifecycle request. With the
// timekeeper it blocks, and a deadline every TK_ROOT_MS returns with no badge
// so the health checks still run; without, it polls as it always did.
static seL4_MessageInfo_t timekeeper_receive(seL4_CPtr endpoint, seL4_Word *badge) {
  if (!tk_ready || !tk_root_ntfn) return IPC_NBRECV(endpoint, badge, root_reply);
  // The deadline is not pushed back by a message that arrives before it.
  static unsigned long long due;
  unsigned long long now = tk_now();
  if (now >= due || due - now > (unsigned long long)TK_ROOT_MS * tk_ticks_per_ms()) {
    due = now + (unsigned long long)TK_ROOT_MS * tk_ticks_per_ms();
    tk_arm_root(TK_ROOT_MS * tk_ticks_per_ms());
  }
  seL4_MessageInfo_t message = IPC_RECV(endpoint, badge, root_reply);
  if (*badge & TK_BADGE) {
    *badge = 0;
    // A guard that died in its first WFI comes back yielding only.
    if (tk_guard && tk_guard_stuck(&tk_watch, &tk_guard_looks, TK_GUARD_STUCK_CHECKS)) {
      tk_watch.wfi = 3;
      tk_guard_looks = 0;
      if (!tk_guard_run()) tk_guard = 0;
    }
  }
  return message;
}
