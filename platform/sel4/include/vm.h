#pragma once
/* What the supervisor's address-space code needs to know about the
   architecture, in one place: the kernel objects, the frame sizes, the
   capability depth, the argument registers and the generic timer. AArch32 is
   what VioletOS ships; every AArch32 definition here is what the code said
   before this header existed, so the 32-bit image is unchanged. AArch64 is
   the port in progress (runtime/arch/README.md): main's 64-bit kernel
   (kernel/sel4/aarch64) at EL1, four-level page tables, 48-bit addresses.

   The page-table side of AArch64 is in memory.h: there the supervisor has
   no per-section bookkeeping any more, a mapping makes the tables it lacks
   (vm_page_map below). */
#include <sel4/sel4.h>

/* The depth of every capability address in a CNode the supervisor names: the
   whole word, as the kernel's guard arithmetic expects. */
#define SEL4_DEPTH seL4_WordBits

/* VIOLET_HOST_MODEL_AARCH64: the host test of memory.h's AArch64 allocator
   (system/supervisor/tests/memory_test.c) on any host, against a model of
   the kernel. Nothing else defines it. */
#if defined(__aarch64__) || defined(VIOLET_HOST_MODEL_AARCH64)
#define VM_AARCH64 1
#else
#define VM_AARCH64 0
#endif

#if VM_AARCH64

/* A process's address space: one top-level table. */
#define VM_VSPACE_OBJECT seL4_ARM_VSpaceObject
/* Arguments a thread is started with (runtime/transport.c process_entry). */
#define VM_ARG0 x0
#define VM_ARG1 x1
/* AArch64 has no 64 KiB frame: a window the AArch32 code maps as one 64 KiB
   frame is sixteen 4 KiB pages. seL4_ARM_LargePageObject is 2 MiB here. */
#define VM_64K_OBJECT seL4_ARM_SmallPageObject
#define VM_64K_STEP 0x1000u
/* The frame sizes, largest first, and their object types. */
#define VM_FRAME_SIZES 3
static const unsigned vm_frame_bits[VM_FRAME_SIZES] = {seL4_HugePageBits, seL4_LargePageBits, seL4_PageBits};
static const unsigned vm_frame_types[VM_FRAME_SIZES] = {seL4_ARM_HugePageObject, seL4_ARM_LargePageObject,
                                                        seL4_ARM_SmallPageObject};
/* Tables below the VSpace: upper directory, directory and page table at EL1
   (48-bit addresses); a kernel built for EL2 with 40-bit physical addresses
   has a larger top level and one level fewer. */
#if defined(CONFIG_ARM_HYPERVISOR_SUPPORT) && defined(CONFIG_ARM_PA_SIZE_BITS_40)
#define VM_TABLE_LEVELS 2
#else
#define VM_TABLE_LEVELS 3
#endif

/* The generic timer. The virtual counter is what the kernel exports to user
   level (KernelArmExportVCNTUser). The physical timer is programmable from
   user level only when the kernel exports it (KernelArmExportPTMRUser);
   otherwise CNTP_* trap, and vm_user_timer says so (timekeeper_thread.h). */
static inline unsigned long long vm_counter(void) {
  unsigned long long value;
  __asm__ volatile("isb; mrs %0, cntvct_el0" : "=r"(value));
  return value;
}
static inline unsigned vm_counter_frequency(void) {
  unsigned long long value;
  __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(value));
  return (unsigned)value;
}
/* Arm the EL0 physical timer `ticks` from now, or stop it with 0. */
static inline void vm_ptimer_program(unsigned ticks) {
  if (ticks) __asm__ volatile("msr cntp_tval_el0, %0" ::"r"((unsigned long long)ticks));
  __asm__ volatile("msr cntp_ctl_el0, %0" ::"r"(ticks ? 1ull : 0ull));
  __asm__ volatile("isb");
}
static inline void vm_wait_for_interrupt(void) { __asm__ volatile("dsb sy; wfi" ::: "memory"); }

/* Map a frame, making the page tables the address still lacks: seL4 refuses
   the mapping with FailedLookup while a level is missing, and PageTable_Map
   installs a table at the first missing level, so each refusal costs one
   table and the last try maps the frame. A 2 MiB or 1 GiB frame stops at
   its own level and never gets a page table under it. Same contract as
   runtime/arch map_frame (host-tested there). `vm_table` is the allocator's
   (memory.h): a fresh seL4_ARM_PageTableObject.

   The supervisor's call sites keep libsel4's name (the macro below), so
   the audit of what they map with which rights (runtime/open/tests/
   test_page_rights.py) reads them as before; this wrapper passes rights and
   attributes through unchanged. */
static seL4_CPtr vm_table(void);
static inline seL4_Error vm_page_map(seL4_CPtr frame, seL4_CPtr vspace, seL4_Word va, seL4_CapRights_t rights,
                                     seL4_ARM_VMAttributes attributes) {
  for (unsigned made = 0;; made++) {
    seL4_Error error = seL4_ARM_Page_Map(frame, vspace, va, rights, attributes);
    if (error != seL4_FailedLookup || made == VM_TABLE_LEVELS) return error;
    error = seL4_ARM_PageTable_Map(vm_table(), vspace, va, seL4_ARM_Default_VMAttributes);
    if (error) return error;
  }
}
#define seL4_ARM_Page_Map(frame, vspace, va, rights, attributes) vm_page_map(frame, vspace, va, rights, attributes)

#else /* AArch32 */

#define VM_VSPACE_OBJECT seL4_ARM_PageDirectoryObject
#define VM_ARG0 r0
#define VM_ARG1 r1
#define VM_64K_OBJECT seL4_ARM_LargePageObject
#define VM_64K_STEP 65536u

static inline unsigned long long vm_counter(void) {
  unsigned lo, hi;
  __asm__ volatile("isb; mrrc p15,1,%0,%1,c14" : "=r"(lo), "=r"(hi));
  return ((unsigned long long)hi << 32) | lo;
}
static inline unsigned vm_counter_frequency(void) {
  unsigned freq;
  __asm__ volatile("mrc p15,0,%0,c14,c0,0" : "=r"(freq));
  return freq;
}
static inline void vm_ptimer_program(unsigned ticks) {
  if (ticks) __asm__ volatile("mcr p15, 0, %0, c14, c2, 0" ::"r"(ticks)); // CNTP_TVAL
  __asm__ volatile("mcr p15, 0, %0, c14, c2, 1" ::"r"(ticks ? 1u : 0u)); // CNTP_CTL
  __asm__ volatile("isb");
}
static inline void vm_wait_for_interrupt(void) { __asm__ volatile("dsb; wfi" ::: "memory"); }

#endif

/* Whether the supervisor's threads may program the physical timer (PPI 30)
   the timekeeper uses (VM_USER_TIMER), and whether every user thread may
   (VM_TIMER_EVERYONE). Both 32-bit kernels export it to every thread
   (KernelArmExportPTMRUser), so an app can stop it: the timekeeper runs with
   its guard, and without the timekeeper the Open Runtime programs it itself.
   The AArch64 kernels give it only to threads with seL4_TCBFlag_ellisTimer
   (kernel/sel4/aarch64/ellis-timer.patch): the root thread from boot, the
   timekeeper by vm_timer_grant; an app's access traps. */
#if defined(CONFIG_EXPORT_PTMR_USER)
#define VM_USER_TIMER 1
#define VM_TIMER_EVERYONE 1
static inline int vm_timer_grant(seL4_CPtr tcb) { (void)tcb; return 0; }
#elif defined(CONFIG_ELLIS_TIMER_GRANT)
#define VM_USER_TIMER 1
#define VM_TIMER_EVERYONE 0
static inline int vm_timer_grant(seL4_CPtr tcb) {
  seL4_TCB_SetFlags_t set = seL4_TCB_SetFlags(tcb, 0, seL4_TCBFlag_ellisTimer);
  return set.error || !(set.flags & seL4_TCBFlag_ellisTimer);
}
#else
#define VM_USER_TIMER 0
#define VM_TIMER_EVERYONE 0
static inline int vm_timer_grant(seL4_CPtr tcb) { (void)tcb; return 1; }
#endif
