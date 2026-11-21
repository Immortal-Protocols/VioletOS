// The C portion of the AArch32 ABI is limited to libsel4 and the virtual
// counter.
#include "tasks.h"
seL4_IPCBuffer *__sel4_ipc_buffer;
char __sel4_print_error;
enum { WORDS = 14 };
extern void component_main(void);
__attribute__((section(".entry"), noreturn)) void process_entry(void) {
  seL4_SetIPCBuffer((void *)IPC_BUFFER);
  clock_init();
  component_main();
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
  read_words(words, seL4_Recv(ep, &badge));
  return badge;
}
void ipc_reply(unsigned *words) { seL4_Reply(send_words(words)); }
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
