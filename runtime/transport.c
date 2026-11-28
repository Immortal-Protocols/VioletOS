// The C portion of the component ABI is limited to libsel4 and the virtual
// counter. It builds for AArch32 and AArch64: the message words stay 32-bit
// values in seL4_Word registers, and only the counter reads and the badge
// width differ.
#include "tasks.h"
#include "mcs.h"
#include "timekeeper.h"
seL4_IPCBuffer *__sel4_ipc_buffer;
char __sel4_print_error;
enum { WORDS = 14 };
// A message word is 32 bits on both architectures (runtime/protocol.rs).
_Static_assert(sizeof(unsigned) == 4, "message words are 32-bit");
extern void component_main(unsigned);
// Whether the supervisor's timekeeper serves this process: it says so in the
// second argument at start (TK_LIVE), and then the page at TK_PAGE and the
// two capabilities are in place. 0: not served. TK_LIVE: served. TK_LIVE | 1:
// served, but Radio is holding the CPU (hold_cpu) and its delays spin, as
// they always did. Never cleared: when the supervisor starts the Open
// Runtime's thread again after a fault, the memory is as it was.
unsigned tk_on;
__attribute__((section(".entry"), noreturn)) void
process_entry(unsigned identity, unsigned timekeeper) {
  seL4_SetIPCBuffer((void *)IPC_BUFFER);
  if (timekeeper) tk_on = timekeeper;
  clock_init();
  component_main(identity);
  for (;;)
    seL4_Yield();
}
static seL4_MessageInfo_t send_words(unsigned *words) {
  for (unsigned i = 0; i < WORDS; i++)
    seL4_SetMR(i, words[i]);
  return seL4_MessageInfo_new(0, 0, 0, WORDS);
}
static void read_words(unsigned *words, seL4_MessageInfo_t tag) {
  unsigned n = seL4_MessageInfo_get_length(tag);
  for (unsigned i = 0; i < WORDS; i++)
    words[i] = i < n ? seL4_GetMR(i) : 0;
}
void ipc_call(unsigned ep, unsigned *words) {
  read_words(words, seL4_Call(ep, send_words(words)));
}
// The kernel stores a whole seL4_Word badge: 8 bytes on AArch64. Badges are
// the supervisor's small numbers, so the low 32 bits are the badge.
unsigned ipc_receive(unsigned ep, unsigned *words) {
  seL4_Word badge = 0;
  read_words(words, IPC_RECV(ep, &badge, REPLY_SLOT));
  return (unsigned)badge;
}
unsigned ipc_try_receive(unsigned ep,unsigned *words) {
  seL4_Word badge=0;
  seL4_MessageInfo_t tag=IPC_NBRECV(ep,&badge,REPLY_SLOT);
  if (seL4_MessageInfo_get_length(tag)!=WORDS) return 0;
  read_words(words,tag);
  return 1;
}
void ipc_reply(unsigned *words) { IPC_REPLY(REPLY_SLOT, send_words(words)); }
unsigned ticks_per_ms;
// The generic timer's virtual counter (CNTVCT) and its frequency (CNTFRQ):
// CP15 on AArch32, system registers on AArch64 (as runtime/arch/mod.rs).
// ticks() is the counter's low 32 bits on both.
uint32_t ticks(void) {
#if defined(__aarch64__)
  uint64_t count;
  __asm__ volatile("isb; mrs %0, cntvct_el0" : "=r"(count));
  return (uint32_t)count;
#else
  uint32_t lo, hi;
  __asm__ volatile("isb; mrrc p15,1,%0,%1,c14" : "=r"(lo), "=r"(hi));
  return lo;
#endif
}
void clock_init(void) {
#if defined(__aarch64__)
  uint64_t freq;
  __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(freq));
  ticks_per_ms = (unsigned)(freq / 1000);
#else
  unsigned freq;
  __asm__ volatile("mrc p15,0,%0,c14,c0,0" : "=r"(freq));
  ticks_per_ms = freq / 1000;
#endif
}
// The timekeeper, for this process (platform/sel4/include/timekeeper.h).
unsigned timekeeper_live(void) { return tk_on; }
// Ask for a signal `ticks` of the counter from now. It arrives on the
// process's notification: TK_WAIT_SLOT, or bound to its thread (Power, the
// Open Runtime), where a receive returns it as a badge.
static inline __attribute__((always_inline)) void tk_arm(unsigned ticks) {
  tk_page_arm((struct tk_page *)TK_PAGE, ticks);
  seL4_Signal(TK_KICK_SLOT);
}
void timekeeper_arm_ms(unsigned ms) {
  if (tk_on) tk_arm(tk_ticks_until(0, (unsigned long long)ms * ticks_per_ms));
}
// Wait for the signal. Nothing is read from it, so the message registers are
// not copied to the IPC buffer as seL4_Wait would.
static void tk_wait(void) {
  seL4_Word badge, info, m0, m1, m2, m3;
#ifdef CONFIG_KERNEL_MCS
  arm_sys_recv(seL4_SysWait, TK_WAIT_SLOT, &badge, &info, &m0, &m1, &m2, &m3, 0);
#else
  arm_sys_recv(seL4_SysRecv, TK_WAIT_SLOT, &badge, &info, &m0, &m1, &m2, &m3, 0);
#endif
}
// The Open Runtime's timer (runtime/open/sel4.c), which has checked tk_on:
// a signal `ticks` from now, or (0) none.
void timekeeper_arm_ticks(unsigned ticks) {
  tk_page_arm((struct tk_page *)TK_PAGE, ticks);
  if (ticks) seL4_Signal(TK_KICK_SLOT);
}
// Power only: TK_OP_SUSPEND with the wake mask, TK_OP_RESUME with the reason.
void timekeeper_command(unsigned op, unsigned arg) {
  if (!tk_on) return;
  tk_page_command((struct tk_page *)TK_PAGE, op, arg);
  seL4_Signal(TK_KICK_SLOT);
}
// TK_FROZEN while the system is suspended to idle; then why it last woke.
unsigned timekeeper_state(void) { return tk_on ? ((struct tk_page *)TK_PAGE)->state : 0; }
unsigned timekeeper_reason(void) { return tk_on ? ((struct tk_page *)TK_PAGE)->reason : 0; }
// A wake source saw its event (a touch, a packet): end a suspend to idle.
// Counts only from a process Power named when it suspended.
void system_wake(unsigned reason) {
  if (!tk_on || !reason) return;
  ((struct tk_page *)TK_PAGE)->wake = reason;
  seL4_Signal(TK_KICK_SLOT);
}
// With the timekeeper the process blocks until the time has passed, so the
// CPU can idle: it asks for a signal after what is left and waits for it. A
// signal left from an earlier sleep only costs a pass of the loop. Without
// the timekeeper (an older supervisor, a timer that does not fire), and
// while Radio holds the CPU, it yields, as it always did.
void delay_ms(unsigned ms) {
  unsigned start = ticks(), span = ms * ticks_per_ms, gone;
  while ((gone = ticks() - start) < span) {
    if (tk_on == TK_LIVE) {
      tk_arm(span - gone);
      tk_wait();
    } else {
      seL4_Yield();
    }
  }
}

/* Radio's WMT handshake polls the connectivity chip's UART, which has a small
   FIFO and no flow control: a thread that preempts Radio mid-exchange (the
   Open Runtime and Touch wake at priority 150) makes it lose bytes. Radio
   holds the CPU above them while the exchange runs, then returns to 100.
   Slot 12 is Radio's own TCB (supervisor.c); no other process calls this. */
enum { SELF_TCB_SLOT = 12 };
void hold_cpu(unsigned on) {
  if (tk_on) tk_on = TK_LIVE | (on != 0);
  seL4_TCB_SetPriority(SELF_TCB_SLOT, SELF_TCB_SLOT, on ? 160 : 100);
}

void worker_signal(unsigned cap) { seL4_Signal(cap); }
// Drop a mapped frame's cached lines in [start, end) after a device wrote it.
int page_invalidate(unsigned cap, unsigned start, unsigned end) {
  return seL4_ARM_Page_Invalidate_Data(cap, start, end);
}
void worker_wait(unsigned cap) { seL4_Wait(cap, 0); }
