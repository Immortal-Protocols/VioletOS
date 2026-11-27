// The AArch64 Linux vDSO ABI uses the same timespec layout on glibc and musl.
// The runtime fills the read-only clock data before the process starts.
#include <stdint.h>
struct time_pair { int64_t sec, subsec; };
struct clock_base { uint64_t start_ticks, epoch_ns; };
__attribute__((section(".violet_clock"), visibility("hidden"), used))
const volatile struct clock_base violet_clock;

static inline uint64_t now_ns(void) {
    uint64_t ticks, hz;
    __asm__ volatile("isb; mrs %0, cntpct_el0" : "=r"(ticks));
    __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(hz));
    ticks -= violet_clock.start_ticks;
    return ticks / hz * 1000000000ULL + ticks % hz * 1000000000ULL / hz;
}

static long read_clock(int clock, struct time_pair *value) {
    switch (clock) {
    case 0: case 5: // Realtime and realtime coarse
        if (!violet_clock.epoch_ns) return -38;
        break;
    case 1: case 4: case 6: case 7: // Monotonic, raw, coarse and boot time
        break;
    default: {
        register long arg0 __asm__("x0") = clock;
        register struct time_pair *arg1 __asm__("x1") = value;
        register long number __asm__("x8") = 113;
        __asm__ volatile("svc #0" : "+r"(arg0) : "r"(arg1), "r"(number) : "cc", "memory");
        return arg0;
    }
    }
    uint64_t ns = now_ns() + ((clock == 0 || clock == 5) ? violet_clock.epoch_ns : 0);
    value->sec = ns / 1000000000ULL;
    value->subsec = ns % 1000000000ULL;
    return 0;
}

long __kernel_clock_gettime(int clock, struct time_pair *value) {
    return read_clock(clock, value);
}

long __kernel_gettimeofday(struct time_pair *value, int timezone[2]) {
    if (value) {
        long result = read_clock(0, value);
        if (result) return result;
        value->subsec /= 1000;
    }
    if (timezone) timezone[0] = timezone[1] = 0;
    return 0;
}
