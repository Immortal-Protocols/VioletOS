// The C portion of the AArch32 ABI is limited to libsel4 and the virtual
// counter.
#include "tasks.h"
#include "mcs.h"
seL4_IPCBuffer *__sel4_ipc_buffer;
char __sel4_print_error;
enum { WORDS = 14 };
extern void component_main(unsigned);
__attribute__((section(".entry"), noreturn)) void
process_entry(unsigned identity) {
  seL4_SetIPCBuffer((void *)IPC_BUFFER);
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
unsigned ipc_receive(unsigned ep, unsigned *words) {
  unsigned badge = 0;
  read_words(words, IPC_RECV(ep, &badge, REPLY_SLOT));
  return badge;
}
unsigned ipc_try_receive(unsigned ep,unsigned *words) {
  unsigned badge=0;
  seL4_MessageInfo_t tag=IPC_NBRECV(ep,&badge,REPLY_SLOT);
  if (seL4_MessageInfo_get_length(tag)!=WORDS) return 0;
  read_words(words,tag);
  return 1;
}
void ipc_reply(unsigned *words) { IPC_REPLY(REPLY_SLOT, send_words(words)); }
unsigned ticks_per_ms;
uint32_t ticks(void) {
  uint32_t lo, hi;
  __asm__ volatile("isb; mrrc p15,1,%0,%1,c14" : "=r"(lo), "=r"(hi));
  return lo;
}
void clock_init(void) {
  unsigned freq;
  __asm__ volatile("mrc p15,0,%0,c14,c0,0" : "=r"(freq));
  ticks_per_ms = freq / 1000;
}
void delay_ms(unsigned ms) {
  unsigned start = ticks();
  while (ticks() - start < ms * ticks_per_ms)
    seL4_Yield();
}

/* Radio's WMT handshake polls the connectivity chip's UART, which has a small
   FIFO and no flow control: a thread that preempts Radio mid-exchange (the
   Open Runtime and Touch wake at priority 150) makes it lose bytes. Radio
   holds the CPU above them while the exchange runs, then returns to 100.
   Slot 12 is Radio's own TCB (supervisor.c); no other process calls this. */
enum { SELF_TCB_SLOT = 12 };
void hold_cpu(unsigned on) { seL4_TCB_SetPriority(SELF_TCB_SLOT, SELF_TCB_SLOT, on ? 160 : 100); }

void worker_signal(unsigned cap) { seL4_Signal(cap); }
// Drop a mapped frame's cached lines in [start, end) after a device wrote it.
int page_invalidate(unsigned cap, unsigned start, unsigned end) {
  return seL4_ARM_Page_Invalidate_Data(cap, start, end);
}
void worker_wait(unsigned cap) { seL4_Wait(cap, 0); }
