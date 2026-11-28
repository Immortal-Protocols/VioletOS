/* Memory-protection probe for `make test-system32` (targets/qemu/test_system.py).
 *
 * Every launch first makes the calls that must be refused without harm
 * (writable+executable memory, data turned into code, system calls writing
 * where the app may not) and prints where its stack, heap and mappings are.
 * Then it does one thing the hardware must stop, a different one each launch
 * (counted in /tmp/wxprobe.n): the runtime ends the app there, since signals
 * are not delivered. The seventh launch does nothing fatal and exits 0.
 */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/mman.h>
#include <sys/random.h>
#include <unistd.h>

static int failures;
static const int constant = 5;

static void expect(int ok, const char *what) {
    if (!ok) {
        failures++;
        printf("wx: FAIL %s (errno %d)\n", what, errno);
    }
}
static int refused(void *p, int error) { return p == MAP_FAILED && errno == error; }

__attribute__((noinline)) static int target(void) { return 7; }
static void *thread(void *arg) { return (char *)arg + target(); }

int main(void) {
    setvbuf(stdout, 0, _IONBF, 0);
    int n = 0;
    unsigned char before_map[16];
    FILE *f = fopen("/tmp/wxprobe.n", "r");
    if (f) { fscanf(f, "%d", &n); fclose(f); }
    f = fopen("/tmp/wxprobe.n", "w");
    if (f) { fprintf(f, "%d\n", n + 1); fclose(f); }

    const int anon = MAP_PRIVATE | MAP_ANONYMOUS;
    unsigned char *code = (unsigned char *)((unsigned long)target & ~1ul);
    void *code_page = (void *)((unsigned long)code & ~4095ul);
    memcpy(before_map, code, 16);

    /* No writable and executable memory, and no new executable memory at all. */
    expect(refused(mmap(0, 65536, PROT_READ | PROT_WRITE | PROT_EXEC, anon, -1, 0), EACCES), "mmap RWX refused");
    expect(refused(mmap(0, 65536, PROT_READ | PROT_EXEC, anon, -1, 0), EACCES), "mmap RX refused");
    unsigned *data = mmap(0, 65536, PROT_READ | PROT_WRITE, anon, -1, 0);
    expect(data != MAP_FAILED, "mmap RW");
    data[0] = 0xe12fff1e; /* bx lr */
    expect(mprotect(data, 65536, PROT_READ | PROT_EXEC) == -1 && errno == EACCES, "mprotect RW->RX refused");
    expect(mprotect(data, 65536, PROT_READ) == 0, "mprotect RW->R");
    expect(mprotect(data, 65536, PROT_READ | PROT_EXEC) == -1 && errno == EACCES, "mprotect R->RX refused");
    expect(mprotect(code_page, 4096, PROT_READ | PROT_WRITE | PROT_EXEC) == -1 && errno == EACCES, "code made RWX refused");
    expect(mprotect(code_page, 4096, PROT_READ | PROT_WRITE) == -1 && errno == EACCES, "part of a code page made writable refused");
    expect(target() == 7, "code still runs");

    /* Nothing of the app's goes into a 64 KiB page that holds code, whatever
     * its protection: the hardware runs the whole page. A read-only mapping
     * of a file the app wrote, placed over or beside the code, must be
     * refused, not filled in and left executable. */
    int shell = open("/tmp/wxprobe.code", O_RDWR | O_CREAT | O_TRUNC, 0600);
    static unsigned ret[1024];
    for (int i = 0; i < 1024; i++) ret[i] = 0xe12fff1e; /* bx lr */
    expect(shell >= 0 && write(shell, ret, sizeof ret) == sizeof ret, "write a file of instructions");
    char *page64 = (char *)((unsigned long)code & ~65535ul);
    char *beside = (char *)code_page == page64 + 61440 ? (char *)code_page - 4096 : (char *)code_page + 4096;
    expect(refused(mmap(code_page, 4096, PROT_READ, MAP_PRIVATE | MAP_FIXED, shell, 0), EACCES), "file mapped over code refused");
    expect(refused(mmap(beside, 4096, PROT_READ, MAP_PRIVATE | MAP_FIXED, shell, 0), EACCES), "file mapped beside code refused");
    expect(refused(mmap(beside, 4096, PROT_NONE, MAP_PRIVATE | MAP_FIXED, shell, 0), EACCES), "PROT_NONE file beside code refused");
    expect(refused(mmap(beside, 4096, PROT_READ, anon | MAP_FIXED, -1, 0), EACCES), "anonymous read-only beside code refused");
    for (char *p = page64; p < page64 + 65536; p += 4096)
        if (p != code_page && p != beside)
            expect(refused(mmap(p, 4096, PROT_READ, MAP_PRIVATE | MAP_FIXED, shell, 0), EACCES), "file mapped in the code's 64 KiB page refused");
    expect(mprotect(beside, 4096, PROT_READ) == -1, "a read-only hole in a page of code refused");
    expect(target() == 7 && memcmp(before_map, code, 16) == 0, "code unchanged by the mappings");

    /* The trampoline page (new threads start there) is the runtime's. Find it:
     * the one candidate where a no-replace mapping says something is there. */
    char *stubs = 0;
    for (unsigned long a = 0xbf100000ul; a < 0xc0000000ul; a += 65536) {
        void *p = mmap((void *)a, 4096, PROT_NONE, anon | MAP_FIXED_NOREPLACE, -1, 0);
        if (p == MAP_FAILED && errno == EEXIST) stubs = (char *)a;
        else if (p != MAP_FAILED) munmap(p, 4096);
    }
    expect(stubs != 0, "found the trampoline page");
    if (stubs) {
        expect(munmap(stubs, 65536) == -1 && errno == EINVAL, "munmap of the trampoline page refused");
        expect(munmap((void *)0xbf100000ul, 0xe00000) == -1 && errno == EINVAL, "munmap of every candidate refused");
        expect(refused(mmap(stubs, 65536, PROT_READ, MAP_PRIVATE | MAP_FIXED, shell, 0), EACCES), "file mapped over the trampoline page refused");
        expect(refused(mmap(stubs + 4096, 4096, PROT_READ, MAP_PRIVATE | MAP_FIXED, shell, 0), EACCES), "file mapped inside the trampoline page refused");
        expect(refused(mmap(stubs, 65536, PROT_READ | PROT_WRITE, anon | MAP_FIXED, -1, 0), EACCES), "trampoline page replaced refused");
        expect(mprotect(stubs, 65536, PROT_READ) == -1 && errno == EACCES, "mprotect of the trampoline page refused");
        lseek(shell, 0, SEEK_SET);
        expect(read(shell, stubs, 8) == -1 && errno == EFAULT, "read() into the trampoline page refused");
    }
    close(shell);
    pthread_t t;
    void *result = 0;
    expect(pthread_create(&t, 0, thread, (void *)100) == 0 && pthread_join(t, &result) == 0 && result == (void *)107,
           "a thread still starts through the trampoline page");

    /* System calls do not write where the app may not. */
    unsigned char before[16];
    memcpy(before, code, 16);
    int fd = open("/apps/wxprobe", O_RDONLY);
    expect(fd >= 0, "open own program");
    expect(read(fd, code, 16) == -1 && errno == EFAULT, "read() into code refused");
    expect(memcmp(before, code, 16) == 0, "code unchanged");
    expect(read(fd, (void *)&constant, 4) == -1 && errno == EFAULT, "read() into constants refused");
    expect(read(fd, data, 16) == -1 && errno == EFAULT, "read() into read-only memory refused");
    expect(getrandom(code, 16, 0) == -1 && errno == EFAULT, "getrandom() into code refused");
    /* A buffer that starts writable and runs into read-only memory: EFAULT
     * with nothing written, not a partial fill. */
    unsigned char *two = mmap(0, 131072, PROT_READ | PROT_WRITE, anon, -1, 0);
    expect(two != MAP_FAILED && mprotect(two + 65536, 65536, PROT_READ) == 0, "half a mapping read-only");
    expect(getrandom(two + 65536 - 512, 1024, 0) == -1 && errno == EFAULT, "getrandom() running into read-only memory refused");
    int touched = 0;
    for (int i = 0; i < 512; i++) touched |= two[65536 - 512 + i];
    expect(!touched, "and nothing was written before the refusal");
    char *hidden = mmap(0, 65536, PROT_READ | PROT_WRITE, anon, -1, 0);
    strcpy(hidden, "kept");
    expect(mprotect(hidden, 65536, PROT_NONE) == 0, "mprotect PROT_NONE");
    expect(read(fd, hidden, 16) == -1 && errno == EFAULT, "read() into PROT_NONE refused");
    expect(write(fd, hidden, 4) == -1 && errno == EFAULT, "write() from PROT_NONE refused");
    expect(mprotect(hidden, 65536, PROT_READ | PROT_WRITE) == 0 && strcmp(hidden, "kept") == 0, "PROT_NONE keeps contents");
    close(fd);

    /* Random numbers: seeded, and different every time. */
    unsigned char a[32], b[32], u[32] = {0};
    expect(getrandom(a, 32, 0) == 32 && getrandom(b, 32, 0) == 32 && memcmp(a, b, 32) != 0, "getrandom");
    fd = open("/dev/urandom", O_RDONLY);
    expect(fd >= 0 && read(fd, u, 32) == 32 && memcmp(u, a, 32) != 0 && memcmp(u, b, 32) != 0, "/dev/urandom");
    close(fd);
    unsigned char *at_random = (unsigned char *)getauxval(AT_RANDOM);
    expect(at_random != 0, "AT_RANDOM");

    if (!failures) printf("wx %d: checks ok\n", n);
    // One write for the line: the runtime's own log lines (a Shield verdict)
    // share the serial port and may land between two writes.
    char line[192];
    int len = snprintf(line, sizeof line, "wx %d: layout main=%p stack=%p heap=%p mmap=%p random=", n, (void *)main, (void *)&n, sbrk(0), (void *)data);
    for (int i = 0; at_random && i < 16; i++) len += snprintf(line + len, sizeof line - len, "%02x", at_random[i]);
    line[len++] = '\n';
    write(1, line, len);

    volatile unsigned stack_code[2] = {0xe12fff1e, 0};
    unsigned *heap_code = malloc(64);
    heap_code[0] = 0xe12fff1e;
    switch (n) {
    case 0:
        printf("wx 0: probe write-code\n");
        *(volatile unsigned char *)code = 0;
        break;
    case 1:
        printf("wx 1: probe write-constant\n");
        *(volatile int *)&constant = 6;
        break;
    case 2:
        printf("wx 2: probe run-stack\n");
        ((void (*)(void))stack_code)();
        break;
    case 3:
        printf("wx 3: probe run-heap\n");
        ((void (*)(void))heap_code)();
        break;
    case 4:
        printf("wx 4: probe read-none\n");
        mprotect(hidden, 65536, PROT_NONE);
        n += *(volatile char *)hidden;
        break;
    case 5:
        /* `data` was written while read-write and made read-only afterwards:
         * the page was mapped all along, so only a change of the hardware
         * mapping in place (and the flush that goes with it) stops this store. */
        printf("wx 5: probe write-readonly\n");
        *(volatile unsigned *)data = 1;
        break;
    default:
        printf("wx %d: all probes done\n", n);
        return failures != 0;
    }
    printf("wx %d: SURVIVED\n", n);
    return 1;
}
