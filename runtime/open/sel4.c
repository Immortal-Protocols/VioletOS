// The seL4 invocations the Open Runtime makes, as plain C functions for Rust.
// Every call returns the seL4 error code (0 on success) unless noted.
#include "tasks.h"
#include "mcs.h"

int open_retype(unsigned untyped, unsigned type, unsigned bits, unsigned root, unsigned slot) {
  return seL4_Untyped_Retype(untyped, type, bits, root, 0, 0, slot, 1);
}
unsigned open_object_type(unsigned which) {
  switch (which) {
  case 0: return seL4_UntypedObject;
  case 1: return seL4_TCBObject;
  case 2: return seL4_EndpointObject;
  case 3: return seL4_CapTableObject;
#if defined(__aarch64__)
  /* The root of an address space. (On AArch64 the old PageDirectory name is
     an alias of PageTable, so it must not be used here.) */
  case 4: return seL4_ARM_VSpaceObject;
  case 5: return seL4_ARM_PageTableObject;
  /* No 64 KiB frame exists on AArch64 (LargePage is 2 MiB there): the
     page64 build is AArch32 only, and asking for one is refused. */
  case 6: return ~0u;
#else
  case 4: return seL4_ARM_PageDirectoryObject;
  case 5: return seL4_ARM_PageTableObject;
  case 6: return seL4_ARM_LargePageObject;
#endif
  case 7: return seL4_ARM_SmallPageObject;
  case 8: return seL4_NotificationObject;  /* 9, 10: Reply, SchedContext (MCS) */
#ifdef CONFIG_KERNEL_MCS
  case 9: return seL4_ReplyObject;
  case 10: return seL4_SchedContextObject;
#endif
  default: return ~0u;
  }
}
/* Map a frame, or change the rights of one already mapped at `va` (the
   kernel rewrites the entries in place and flushes the address space's TLB).
   Read-only unless `writable`; Execute-Never unless `exec`. The callers never
   ask for both (W^X); if one did, the mapping would still not be executable. */
int open_page_map(unsigned frame, unsigned pd, unsigned va, int writable, int cached, int exec) {
  seL4_CapRights_t rights = writable ? seL4_AllRights : seL4_CanRead;
  unsigned attributes = cached ? seL4_ARM_Default_VMAttributes : 0;
  if (!exec || writable) attributes |= seL4_ARM_ExecuteNever;
  return seL4_ARM_Page_Map(frame, pd, va, rights, (seL4_ARM_VMAttributes)attributes);
}
int open_page_unmap(unsigned frame) { return seL4_ARM_Page_Unmap(frame); }
int open_page_unify(unsigned frame, unsigned bytes) {
  return seL4_ARM_Page_Unify_Instruction(frame, 0, bytes);
}
int open_pt_map(unsigned pt, unsigned pd, unsigned va) {
  return seL4_ARM_PageTable_Map(pt, pd, va, seL4_ARM_Default_VMAttributes);
}
int open_asid_assign(unsigned pool, unsigned pd) { return seL4_ARM_ASIDPool_Assign(pool, pd); }
int open_cnode_copy(unsigned root, unsigned dest, unsigned bits, unsigned src_root,
                    unsigned src, unsigned src_bits) {
  return seL4_CNode_Copy(root, dest, bits, src_root, src, src_bits, seL4_AllRights);
}
int open_cnode_mint(unsigned root, unsigned dest, unsigned bits, unsigned src_root,
                    unsigned src, unsigned src_bits, unsigned badge) {
  return seL4_CNode_Mint(root, dest, bits, src_root, src, src_bits, seL4_AllRights, badge);
}
int open_cnode_delete(unsigned root, unsigned slot, unsigned bits) {
  return seL4_CNode_Delete(root, slot, bits);
}
int open_cnode_revoke(unsigned root, unsigned slot, unsigned bits) {
  return seL4_CNode_Revoke(root, slot, bits);
}
int open_cnode_move(unsigned root, unsigned dest, unsigned bits, unsigned src, unsigned src_bits) {
  return seL4_CNode_Move(root, dest, bits, root, src, src_bits);
}
/* Whether the kernel has MCS scheduling contexts and Reply objects. */
int open_is_mcs(void) {
#ifdef CONFIG_KERNEL_MCS
  return 1;
#else
  return 0;
#endif
}
/* Keep the caller of the last fault so it can be answered later. Under MCS
   it is in the Reply object at REPLY_SLOT; sys.rs moves that object to `slot`
   and puts an idle one in REPLY_SLOT for the next receive. */
int open_save_caller(unsigned root, unsigned slot, unsigned bits) {
#ifdef CONFIG_KERNEL_MCS
  return seL4_CNode_Move(root, slot, bits, root, REPLY_SLOT, bits);
#else
  return seL4_CNode_SaveCaller(root, slot, bits);
#endif
}
/* A thread's spaces and, without MCS, its fault endpoint (in the thread's
   own CSpace). Under MCS the fault endpoint comes with its scheduling
   context (open_tcb_schedule). */
int open_tcb_configure(unsigned tcb, unsigned fault_ep, unsigned cspace, unsigned cspace_bits,
                       unsigned vspace) {
  seL4_Word guard = seL4_CNode_CapData_new(0, seL4_WordBits - cspace_bits).words[0];
#ifdef CONFIG_KERNEL_MCS
  (void)fault_ep;
  return seL4_TCB_Configure(tcb, cspace, guard, vspace, 0, 0, seL4_CapNull);
#else
  return seL4_TCB_Configure(tcb, fault_ep, cspace, guard, vspace, 0, 0, seL4_CapNull);
#endif
}
/* MCS: give `tcb` scheduling context `sc` with `budget_us` of every
   `period_us` (equal: round robin) at `priority`, faulting to `fault_ep` (a
   capability in the Open Runtime's CSpace). Reconfiguring a running thread's
   context is how it moves between tiers. */
/* Cores the kernel was built for. */
unsigned open_cores(void) { return CONFIG_MAX_NUM_NODES; }
/* Without MCS: `priority`, and with more than one core, `core`. */
int open_tcb_schedule(unsigned tcb, unsigned authority, unsigned priority, unsigned sc,
                      unsigned fault_ep, unsigned control, unsigned budget_us, unsigned period_us,
                      unsigned core) {
#ifdef CONFIG_KERNEL_MCS
  int e = seL4_SchedControl_ConfigureFlags(control, sc, budget_us, period_us, 0, 0, 0);
  if (e) return e;
  if (fault_ep) return seL4_TCB_SetSchedParams(tcb, authority, 0, priority, sc, fault_ep);
  return seL4_TCB_SetPriority(tcb, authority, priority);
#else
  (void)sc; (void)fault_ep; (void)control; (void)budget_us; (void)period_us;
  int e = seL4_TCB_SetPriority(tcb, authority, priority);
#ifdef CONFIG_ENABLE_SMP_SUPPORT
  if (!e) e = seL4_TCB_SetAffinity(tcb, core);
#else
  (void)core;
#endif
  return e;
#endif
}
int open_tcb_priority(unsigned tcb, unsigned authority, unsigned priority) {
  return seL4_TCB_SetPriority(tcb, authority, priority);
}
#if defined(__aarch64__)
/* AArch64: the whole seL4_UserContext (36 words: pc, sp, spsr, x0..x8, x16,
   x17, x18, x29, x30, x9..x15, x19..x28, tpidr_el0, tpidrro_el0), as
   runtime/arch Context holds it. */
_Static_assert(sizeof(seL4_UserContext) == 36 * sizeof(seL4_Word), "runtime/arch MAX_CONTEXT");
int open_tcb_write64(unsigned tcb, int resume, const seL4_Word *regs) {
  seL4_UserContext context;
  __builtin_memcpy(&context, regs, sizeof(context));
  return seL4_TCB_WriteRegisters(tcb, resume, 0, sizeof(context) / sizeof(seL4_Word), &context);
}
int open_tcb_read64(unsigned tcb, seL4_Word *regs) {
  seL4_UserContext context;
  int error = seL4_TCB_ReadRegisters(tcb, 0, 0, sizeof(context) / sizeof(seL4_Word), &context);
  __builtin_memcpy(regs, &context, sizeof(context));
  return error;
}
/* The AArch32 19-word form would overrun its buffer here: refused. */
int open_tcb_write(unsigned tcb, int resume, const unsigned *regs) {
  (void)tcb; (void)resume; (void)regs;
  return seL4_InvalidArgument;
}
int open_tcb_read(unsigned tcb, unsigned *regs) {
  (void)tcb; (void)regs;
  return seL4_InvalidArgument;
}
#else
// regs: pc, sp, cpsr, r0, r1, r8, r9, r10, r11, r12, r2..r7, r14, tpidrurw, tpidruro.
int open_tcb_write(unsigned tcb, int resume, const unsigned *regs) {
  seL4_UserContext context;
  __builtin_memcpy(&context, regs, sizeof(context));
  return seL4_TCB_WriteRegisters(tcb, resume, 0, sizeof(context) / sizeof(seL4_Word), &context);
}
int open_tcb_read(unsigned tcb, unsigned *regs) {
  seL4_UserContext context;
  int error = seL4_TCB_ReadRegisters(tcb, 0, 0, sizeof(context) / sizeof(seL4_Word), &context);
  __builtin_memcpy(regs, &context, sizeof(context));
  return error;
}
#endif
// The thread pointer (TPIDRURO). Unlike a register write, this keeps the Thumb bit.
int open_tcb_set_tls(unsigned tcb, unsigned base) { return seL4_TCB_SetTLSBase(tcb, base); }
int open_tcb_suspend(unsigned tcb) { return seL4_TCB_Suspend(tcb); }
int open_tcb_resume(unsigned tcb) { return seL4_TCB_Resume(tcb); }

// Receive the next fault or message. Returns the label and length; fills badge
// and up to 16 message registers.
unsigned open_receive(unsigned ep, int block, unsigned *badge, unsigned *mrs) {
  seL4_Word b = 0;
  seL4_MessageInfo_t info = block ? IPC_RECV(ep, &b, REPLY_SLOT) : IPC_NBRECV(ep, &b, REPLY_SLOT);
  unsigned length = seL4_MessageInfo_get_length(info);
  for (unsigned i = 0; i < length && i < 16; i++) mrs[i] = seL4_GetMR(i);
  *badge = b;
  return (seL4_MessageInfo_get_label(info) << 16) | length;
}
static seL4_MessageInfo_t load(unsigned label, const unsigned *mrs, unsigned length) {
  for (unsigned i = 0; i < length; i++) seL4_SetMR(i, mrs[i]);
  return seL4_MessageInfo_new(label, 0, 0, length);
}
// Answer the faulting thread that sent the last message.
void open_reply(const unsigned *mrs, unsigned length) { IPC_REPLY(REPLY_SLOT, load(0, mrs, length)); }
// Answer a thread whose reply capability was saved with open_save_caller.
void open_send(unsigned cap, const unsigned *mrs, unsigned length) {
  seL4_Send(cap, load(0, mrs, length));
}
void open_yield(void) { seL4_Yield(); }
#if defined(__aarch64__)
/* AArch64: fault messages carry 64-bit addresses and registers, so these
   move whole words (the 32-bit forms above would cut them). */
unsigned open_receive64(unsigned ep, int block, seL4_Word *badge, seL4_Word *mrs) {
  seL4_Word b = 0;
  seL4_MessageInfo_t info = block ? IPC_RECV(ep, &b, REPLY_SLOT) : IPC_NBRECV(ep, &b, REPLY_SLOT);
  unsigned length = seL4_MessageInfo_get_length(info);
  for (unsigned i = 0; i < length && i < 16; i++) mrs[i] = seL4_GetMR(i);
  *badge = b;
  return (seL4_MessageInfo_get_label(info) << 16) | length;
}
static seL4_MessageInfo_t load64(const seL4_Word *mrs, unsigned length) {
  for (unsigned i = 0; i < length; i++) seL4_SetMR(i, mrs[i]);
  return seL4_MessageInfo_new(0, 0, 0, length);
}
void open_reply64(const seL4_Word *mrs, unsigned length) { IPC_REPLY(REPLY_SLOT, load64(mrs, length)); }
void open_send64(unsigned cap, const seL4_Word *mrs, unsigned length) { seL4_Send(cap, load64(mrs, length)); }
#endif

/* Write back the data cache over [start, end) of the Open Runtime's own
   address space; the range must lie within one page. */
int open_clean(unsigned pd, unsigned start, unsigned end) {
#if defined(__aarch64__)
  return seL4_ARM_VSpace_Clean_Data(pd, start, end);
#else
  return seL4_ARM_PageDirectory_Clean_Data(pd, start, end);
#endif
}

/* Copy n bytes (a multiple of 4, both pointers 4-byte aligned): 32 bytes per
   ldm/stm pair, prefetching ahead. The Open Runtime is built for size, and
   its memcpy moved about 350 MB/s on the handset, 13 ms per full frame. */
void open_copy(void *dst, const void *src, unsigned n) {
  unsigned *d = dst;
  const unsigned *s = src;
  unsigned blocks = n / 32;
#if defined(__aarch64__)
  if (blocks) {
    __asm__ volatile("1:\n"
                     "prfm pldl1keep, [%[s], #256]\n"
                     "ldp x9, x10, [%[s]], #16\n"
                     "ldp x11, x12, [%[s]], #16\n"
                     "stp x9, x10, [%[d]], #16\n"
                     "stp x11, x12, [%[d]], #16\n"
                     "subs %w[b], %w[b], #1\n"
                     "b.ne 1b\n"
                     : [d] "+r"(d), [s] "+r"(s), [b] "+r"(blocks)
                     :
                     : "x9", "x10", "x11", "x12", "memory", "cc");
  }
#else
  if (blocks) {
    __asm__ volatile("1:\n"
                     "pld [%[s], #256]\n"
                     "ldmia %[s]!, {r3, r4, r5, r6, r7, r8, r9, r10}\n"
                     "stmia %[d]!, {r3, r4, r5, r6, r7, r8, r9, r10}\n"
                     "subs %[b], %[b], #1\n"
                     "bne 1b\n"
                     : [d] "+r"(d), [s] "+r"(s), [b] "+r"(blocks)
                     :
                     : "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "memory", "cc");
  }
#endif
  for (n %= 32; n >= 4; n -= 4)
    *d++ = *s++;
}

/* The Open Runtime's timer (see sys.rs): the nonsecure physical timer, which
   the kernel exports to user mode, counting down `ticks` of the counter. */
/* With the supervisor's timekeeper (runtime/transport.c, timekeeper.h) the
   timer is the timekeeper's: a deadline is written to the shared page and
   signalled on the same notification with the same badge, and there is no
   interrupt to acknowledge (the timer slot then holds the kick). */
extern unsigned tk_on;
extern void timekeeper_arm_ticks(unsigned ticks);
void open_timer_arm(unsigned ticks) {
  if (tk_on) return timekeeper_arm_ticks(ticks);
#if defined(__aarch64__)
  /* Needs KernelArmExportPTMRUser, as on AArch32. */
  __asm__ volatile("msr cntp_tval_el0, %0" ::"r"((seL4_Word)ticks));
  __asm__ volatile("msr cntp_ctl_el0, %0" ::"r"((seL4_Word)1));
#else
  __asm__ volatile("mcr p15, 0, %0, c14, c2, 0" ::"r"(ticks)); /* CNTP_TVAL */
  __asm__ volatile("mcr p15, 0, %0, c14, c2, 1" ::"r"(1u));    /* CNTP_CTL: enabled */
#endif
  __asm__ volatile("isb");
}
void open_timer_stop(void) {
  if (tk_on) return timekeeper_arm_ticks(0);
#if defined(__aarch64__)
  __asm__ volatile("msr cntp_ctl_el0, %0" ::"r"((seL4_Word)0));
#else
  __asm__ volatile("mcr p15, 0, %0, c14, c2, 1" ::"r"(0u));
#endif
  __asm__ volatile("isb");
}
int open_irq_ack(unsigned handler) { return tk_on ? 0 : seL4_IRQHandler_Ack(handler); }
/* Wait on the notification alone (app faults stay queued on the endpoint). */
unsigned open_wait(unsigned notification) {
  seL4_Word badge = 0;
  seL4_Wait(notification, &badge);
  return badge;
}
unsigned open_poll(unsigned notification) {
  seL4_Word badge = 0;
  seL4_Poll(notification, &badge);
  return badge;
}
