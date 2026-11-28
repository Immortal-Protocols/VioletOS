/* A 64-bit C program under the Open Runtime on AArch64 (targets/qemu/
 * test_system.py --arch aarch64 opens it from Home, twice):
 *
 * - fork without CLONE_VM: the child gets a copy of the caller's memory (a
 *   global, the heap, the stack) and of its FP/SIMD registers (through the
 *   A64 fork gate), writes to its copy and exits with a status; the parent's
 *   memory is unchanged and wait reports the status;
 * - the 64-bit layout (vma::Layout64): the program, its stack, heap and
 *   mmap areas all above 4 GiB (their addresses are printed, and the test
 *   compares two launches: each part moves); a 64 GiB PROT_NONE reservation;
 *   mmap at a fixed address far above 4 GiB, a system call copying from it,
 *   and the same call from its low 32 bits refused (EFAULT), not aliased;
 *   W^X there as everywhere. */
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static int counter = 1;

static double scaled(double v) {
    /* Keeps the value in a SIMD register across the call. */
    __asm__ volatile("" : "+w"(v));
    return v * 2.0;
}

int main(void) {
    int *heap = malloc(sizeof *heap), st = 0, local = 3;
    *heap = 10;
    register double kept __asm__("d9") = 1.5; /* callee-saved: must survive fork */
    __asm__ volatile("" : "+w"(kept));
    pid_t pid = fork();
    __asm__ volatile("" : "+w"(kept));
    if (pid == 0) {
        counter = 2;
        *heap = 20;
        local = 5;
        _exit(counter + *heap + local + (kept == 1.5 ? 0 : 100)); /* 27 */
    }
    pid_t w = waitpid(pid, &st, 0);
    printf("c64: fork %d child exit %d, parent kept %d %d %d, fp %d\n", pid > 0 && w == pid,
           WIFEXITED(st) ? WEXITSTATUS(st) : -1, counter, *heap, local, scaled(kept) == 3.0);

    /* The layout: every part above 4 GiB. */
    void *map = mmap(0, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uintptr_t code = (uintptr_t)&main, stack = (uintptr_t)&local, brk = (uintptr_t)sbrk(0), area = (uintptr_t)map;
    printf("c64: layout main=%#lx stack=%#lx heap=%#lx mmap=%#lx\n", (unsigned long)code, (unsigned long)stack,
           (unsigned long)brk, (unsigned long)area);
    uintptr_t low = (uintptr_t)1 << 32;
    printf("c64: above 4 GiB %d\n", map != MAP_FAILED && code > low && stack > low && brk > low && area > low);
    void *big = mmap(0, (size_t)64 << 30, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    printf("c64: reserve 64 GiB %d\n", big != MAP_FAILED && (uintptr_t)big > low);

    /* A fixed mapping far above 4 GiB: written, copied from by a call, and
     * its low 32 bits are no address of the app's. */
    char *at = (char *)(uintptr_t)0x4321000000ull;
    char *got = mmap(at, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    int p[2];
    pipe(p);
    memcpy(got, "high", 4);
    ssize_t whole = write(p[1], got, 4);
    char back[4] = {0};
    read(p[0], back, 4);
    errno = 0;
    ssize_t cut = write(p[1], (void *)((uintptr_t)got & 0xffffffffu), 4);
    printf("c64: fixed %d copy %d '%.4s', low word %d errno %d\n", got == at, (int)whole, back, (int)cut, errno);
    /* W^X: no writable and executable memory, no data made code. */
    void *wx = mmap(0, 65536, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int made_code = mprotect(got, 65536, PROT_READ | PROT_EXEC);
    printf("c64: wx refused %d %d\n", wx == MAP_FAILED && errno == EACCES, made_code == -1 && errno == EACCES);
    printf("c64 done\n");
    return 0;
}
