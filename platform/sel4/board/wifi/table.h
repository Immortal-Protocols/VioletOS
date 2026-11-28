// The whole secure-monitor interface of the Ellis kernel extension: four
// operations whose monitor arguments are fixed here. A caller selects among
// them (which register of which of two regions, which of the two programming
// steps of the one window); no value it supplies reaches the monitor.
// Shared by the kernel (invoke.h), the supervisor (user.h) and the host test
// (targets/moto-g-pure/test_ellis_smc.c). No dependencies.
#ifndef ELLIS_SMC_TABLE_H
#define ELLIS_SMC_TABLE_H
enum {
    ELLIS_EMI_READ = 0x4d1,   // (register 0-3, region): one word, primary reader
    ELLIS_EMI_READ_OFFSET,    // (register 0-3, region): one word, offset reader
    ELLIS_EMI_SET,            // (step 0-1, interface 0-1): monitor status
    ELLIS_RANDOM,             // (): the monitor's four reply words
};
enum { ELLIS_REGION_RADIO = 25, ELLIS_REGION_SECURE = 29 };
// The one window that may be programmed: region 25, 7 MiB at 0x42000000,
// read-write for every domain in the upper permission word and for all but
// domains 0 and 2 in the lower one (windows.h radio_permissions).
#define ELLIS_WINDOW_START 0x42000000u
#define ELLIS_WINDOW_SIZE 0x700000u
#define ELLIS_PERMISSIONS_HIGH 0x00b6db6du
#define ELLIS_PERMISSIONS_LOW 0x00b6da28u
#define ELLIS_SET_START ((ELLIS_REGION_RADIO << 24) | (ELLIS_WINDOW_START >> 16))
#define ELLIS_SET_END ((ELLIS_WINDOW_START + ELLIS_WINDOW_SIZE - 1) >> 16)

// 1 and the monitor arguments in m[] when (label, length, a, b) is one of the
// permitted calls; 0 otherwise. m[] for a read by call is (operation 2,
// register selector, region, upper word); by offset, the register offset;
// for a set, (start, end, permissions).
static inline int ellis_smc_arguments(unsigned label, unsigned length, unsigned a, unsigned b, unsigned m[4])
{
    m[0] = m[1] = m[2] = m[3] = 0;
    if (label == ELLIS_RANDOM) return length == 0;
    if (length != 2) return 0;
    if (label == ELLIS_EMI_READ || label == ELLIS_EMI_READ_OFFSET) {
        if (a > 3 || (b != ELLIS_REGION_RADIO && b != ELLIS_REGION_SECURE)) return 0;
        if (label == ELLIS_EMI_READ) {
            m[0] = 2, m[1] = a < 2 ? a : 2, m[2] = b, m[3] = a == 3;
        } else {
            m[0] = (a + 1) * 0x100 + b * 4;
        }
        return 1;
    }
    if (label == ELLIS_EMI_SET) {
        if (a > 1 || b > 1) return 0;
        // Step 0 writes the upper permissions; step 1 the lower ones and the lock bit.
        m[0] = ELLIS_SET_START;
        m[1] = a ? ELLIS_SET_END : ELLIS_SET_END | (1u << 24);
        m[2] = a ? ELLIS_PERMISSIONS_LOW | (1u << 31) : ELLIS_PERMISSIONS_HIGH;
        return 1;
    }
    return 0;
}
#endif
