// Ellis AArch32 board extension. Invoked only through the dedicated root cap.
// The kernel does one thing: it turns a permitted (label, a, b) into one
// secure-monitor call with the fixed arguments of table.h and hands back the
// monitor's reply. When to call, and what the replies mean, is decided at
// user level by the supervisor (wifi/user.h, wifi/windows.h).
#include "table.h"

extern unsigned moto_emi_call(unsigned operation, unsigned x, unsigned y, unsigned z);
extern unsigned moto_emi_read_offset(unsigned offset);
extern unsigned moto_emi_set_window(unsigned start, unsigned end, unsigned permissions);
extern void moto_random_words(unsigned words[4]);
#ifdef CONFIG_ELLIS_WIFI_FIXTURE
void moto_random_words(unsigned words[4]) { for (unsigned i = 0; i < 4; ++i) words[i] = 0; }
// QEMU has no MediaTek firmware. Simulate unsupported reads and reject writes.
unsigned moto_emi_read_offset(unsigned offset) { (void)offset; return ~0u; }
unsigned moto_emi_set_window(unsigned s, unsigned e, unsigned p) {
    (void)s; (void)e; (void)p; return ~0u;
}
unsigned moto_emi_call(unsigned op, unsigned x, unsigned y, unsigned z) {
    (void)op; (void)x; (void)y; (void)z; return ~0u;
}
#endif

static exception_t ellisWifiInvocation(word_t label, word_t length, bool_t call)
{
    tcb_t *thread = NODE_STATE(ksCurThread);
    // Both arguments and all four reply words travel in message registers.
    unsigned a = getSyscallArg(0, NULL), b = getSyscallArg(1, NULL), m[4], reply[4], count = 1;
    if (!call || !ellis_smc_arguments(label, length, a, b, m)) {
        current_syscall_error.type = seL4_IllegalOperation;
        return EXCEPTION_SYSCALL_ERROR;
    }
    if (label == ELLIS_EMI_READ) {
        reply[0] = moto_emi_call(m[0], m[1], m[2], m[3]);
    } else if (label == ELLIS_EMI_READ_OFFSET) {
        reply[0] = moto_emi_read_offset(m[0]);
    } else if (label == ELLIS_EMI_SET) {
        reply[0] = b ? moto_emi_set_window(m[0], m[1], m[2]) : moto_emi_call(0, m[0], m[1], m[2]);
    } else {
        moto_random_words(reply);
        count = 4;
    }
    setRegister(thread, badgeRegister, 0);
    for (unsigned i = 0; i < count; ++i) setRegister(thread, msgRegisters[i], reply[i]);
    setRegister(thread, msgInfoRegister, wordFromMessageInfo(seL4_MessageInfo_new(0, 0, 0, count)));
    setThreadState(thread, ThreadState_Running);
    return EXCEPTION_NONE;
}
