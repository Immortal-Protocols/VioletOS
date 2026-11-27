#define _GNU_SOURCE
#include <elf.h>
#include <stdio.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static long long ns(struct timespec t) { return (long long)t.tv_sec * 1000000000 + t.tv_nsec; }
int main(void) {
    const Elf64_Ehdr *eh = (const void *)getauxval(AT_SYSINFO_EHDR);
    if (!eh || memcmp(eh->e_ident, ELFMAG, SELFMAG) || eh->e_type != ET_DYN) return 1;
    const int clocks[] = {CLOCK_REALTIME, CLOCK_MONOTONIC, CLOCK_BOOTTIME, CLOCK_MONOTONIC_RAW};
    for (unsigned i = 0; i < sizeof(clocks) / sizeof(*clocks); ++i) {
        struct timespec before, fast, after;
        if (syscall(SYS_clock_gettime, clocks[i], &before) || clock_gettime(clocks[i], &fast) ||
            syscall(SYS_clock_gettime, clocks[i], &after) || ns(fast) < ns(before) || ns(fast) > ns(after)) return 2;
    }
    struct timeval tv;
    struct timespec past, now;
    if (gettimeofday(&tv, NULL) || clock_gettime(CLOCK_REALTIME, &now) || now.tv_sec - tv.tv_sec > 1) return 3;
    clock_gettime(CLOCK_MONOTONIC, &past);
    for (unsigned i = 0; i < 80000; ++i) if (clock_gettime(CLOCK_MONOTONIC, &now)) return 4;
    printf("80000 libc clock_gettime: %lld us\n", (ns(now) - ns(past)) / 1000);
    clock_gettime(CLOCK_MONOTONIC, &past);
    for (unsigned i = 0; i < 80000; ++i) if (syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &now)) return 5;
    printf("80000 direct clock syscalls: %lld us\n", (ns(now) - ns(past)) / 1000);
    puts("AArch64 vDSO realtime and monotonic clocks match the kernel");
    return 0;
}
