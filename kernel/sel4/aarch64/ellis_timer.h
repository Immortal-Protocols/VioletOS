/*
 * VioletOS (kernel/sel4/aarch64/ellis-timer.patch): the EL0 physical timer
 * (CNTP_CTL_EL0, CNTP_TVAL_EL0, CNTP_CVAL_EL0; PPI 30) belongs to the threads
 * whose TCB carries seL4_TCBFlag_ellisTimer, and to no other.
 *
 * CNTKCTL_EL1.EL0PTEN is set while such a thread runs and clear otherwise, so
 * any other thread's access to those registers traps (EC 0x18) and reaches
 * its fault handler as a user exception. The kernel's own tick is the virtual
 * timer (CNTV_*, PPI 27) at EL1, which EL0PTEN does not affect. The root task
 * starts with the flag (src/kernel/boot.c); TCB SetFlags changes it only when
 * the invoking thread has it (src/object/tcb.c). Every switch of the running
 * thread applies the target's flag: switchToThread (Arch_switchToThread) and
 * the IPC fastpath (switchToThread_fp). The exception return to EL0 that
 * follows each write is the context synchronisation the write needs.
 */
#pragma once

#include <config.h>

#ifdef CONFIG_ELLIS_TIMER_GRANT
#include <types.h>
#include <object/structures.h>
#include <mode/machine.h>

#define ELLIS_CNTKCTL_EL0PTEN BIT(9)

static inline void ellisTimerAccess(tcb_t *thread)
{
    word_t was, want;
    MRS("CNTKCTL_EL1", was);
    if (thread->tcbFlags & seL4_TCBFlag_ellisTimer) {
        want = was | ELLIS_CNTKCTL_EL0PTEN;
    } else {
        want = was & ~ELLIS_CNTKCTL_EL0PTEN;
    }
    if (want != was) {
        MSR("CNTKCTL_EL1", want);
    }
}
#endif /* CONFIG_ELLIS_TIMER_GRANT */
