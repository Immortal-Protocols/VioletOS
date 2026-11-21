// Ellis AArch32 board extension. Invoked only through the dedicated root cap.
// Label 1 inspects the fixed windows. Label 2 additionally prepares empty ones.
#include "windows.h"

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
    if (!call || !(((label == 1 || label == 2) && length == 1) || (label == 3 && length == 0))) {
        current_syscall_error.type = seL4_IllegalOperation;
        return EXCEPTION_SYSCALL_ERROR;
    }
    tcb_t *thread = NODE_STATE(ksCurThread);
    word_t *ipc = lookupIPCBuffer(true, thread);
    if (!ipc) {
        current_syscall_error.type = seL4_TruncatedMessage;
        return EXCEPTION_SYSCALL_ERROR;
    }
    if (label == 3) {
        unsigned words[4];
        moto_random_words(words);
        // Firmware error replies return one status word. Never use these as entropy.
        unsigned count = words[1] && words[2] && words[3] &&
                         !(words[1] == words[2] && words[2] == words[3]) ? 4 : 0;
        setRegister(thread, badgeRegister, 0);
        for (unsigned i = 0; i < count; ++i) setMR(thread, ipc, i, words[i]);
        setRegister(thread, msgInfoRegister, wordFromMessageInfo(seL4_MessageInfo_new(0, 0, 0, count)));
        setThreadState(thread, ThreadState_Running);
        return EXCEPTION_NONE;
    }
    word_t reader = getSyscallArg(0, lookupIPCBuffer(false, thread));
    if (reader > WIFI_OFFSET) {
        current_syscall_error.type = seL4_IllegalOperation;
        return EXCEPTION_SYSCALL_ERROR;
    }
    word_t result = radio_windows_request(label == 2, reader);
    setRegister(thread, badgeRegister, 0);
    setMR(thread, ipc, 0, result);
    for (unsigned i = 0; i < 8; ++i) {
        setMR(thread, ipc, i + 1, radio_window_raw[i / 4][i % 4]);
    }
    setRegister(thread, msgInfoRegister, wordFromMessageInfo(seL4_MessageInfo_new(0, 0, 0, 9)));
    setThreadState(thread, ThreadState_Running);
    return EXCEPTION_NONE;
}
