/* The Open Runtime's process and signal limits (make test-system32): fork keeps
 * the caller's VFP registers (d8 is kept across calls) and FPSCR, a 1 us
 * interval timer fires at most once a millisecond, an unaligned LDRD is
 * SIGBUS with BUS_ADRALN, SIGKILL ends a vfork child that sleeps or spins
 * (the parent goes on), zombies are capped, vfork in a loop does not run
 * out of thread slots, a "#!" interpreter is checked like any path, and an
 * execve the loader would refuse fails before the caller is torn down. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <fenv.h>
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static double d8_get(void) {
    double v;
    __asm__ volatile("vmov.f64 %P0, d8" : "=w"(v));
    return v;
}

static void vfp_fork(void) {
    double in = 2.5;
    __asm__ volatile("vmov.f64 d8, %P0" ::"w"(in) : "d8");
    fesetround(FE_UPWARD);
    pid_t pid = fork();
    double out = d8_get();
    int up = fegetround() == FE_UPWARD;
    /* Compared as bits in core registers: a 2.5 kept in another VFP register would be lost too. */
    uint64_t bits;
    memcpy(&bits, &out, sizeof bits);
    int kept = bits == 0x4004000000000000ull;
    if (pid == 0) _exit(kept && up ? 0 : 1);
    int st = 0;
    waitpid(pid, &st, 0);
    fesetround(FE_TONEAREST);
    printf("charden: fork kept vfp child %d parent %d\n", WIFEXITED(st) && WEXITSTATUS(st) == 0, kept && up);
}

static volatile sig_atomic_t ticks;
static void on_alarm(int s) { (void)s; ticks++; }
static uint64_t now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000000u + t.tv_nsec;
}

static void timer_floor(void) {
    signal(SIGALRM, on_alarm);
    struct itimerval it = {{0, 1}, {0, 1}}, off = {{0, 0}, {0, 0}};
    setitimer(ITIMER_REAL, &it, 0);
    uint64_t start = now_ns();
    while (now_ns() - start < 100000000u) {}
    setitimer(ITIMER_REAL, &off, 0);
    struct itimerval got;
    getitimer(ITIMER_REAL, &got);
    /* 100 ms at one expiry a millisecond at most (and some at all). */
    printf("charden: 1us timer floor %d\n", ticks >= 1 && ticks <= 120);
}

static sigjmp_buf back;
static volatile int bus_code;
static volatile void *bus_addr;
static void on_bus(int s, siginfo_t *si, void *uc) {
    (void)s; (void)uc;
    bus_code = si->si_code;
    bus_addr = si->si_addr;
    siglongjmp(back, 1);
}

/* LDRD needs a word-aligned address even where LDR does not. */
static void ldrd(const void *p) {
    __asm__ volatile("ldrd r2, r3, [%0]" ::"r"(p) : "r2", "r3", "memory");
}

static void sigbus(void) {
    static uint32_t words[4] = {1, 2, 3, 4};
    struct sigaction sa = {0};
    sa.sa_sigaction = on_bus;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGBUS, &sa, 0);
    char *odd = (char *)words + 1;
    if (!sigsetjmp(back, 1)) {
        ldrd(odd);
        printf("charden: unaligned ldrd did not fault\n");
    }
    printf("charden: SIGBUS code %d addr %d\n", bus_code, bus_addr == odd);
    /* Without a handler the wait status says SIGBUS. */
    signal(SIGBUS, SIG_DFL);
    pid_t pid = fork();
    if (pid == 0) {
        ldrd(odd);
        _exit(0);
    }
    int st = 0;
    waitpid(pid, &st, 0);
    printf("charden: uncaught SIGBUS signal %d\n", WIFSIGNALED(st) ? WTERMSIG(st) : 0);
}

static volatile pid_t vchild;
static void *killer(void *arg) {
    (void)arg;
    while (!vchild) {}
    struct timespec t = {0, 200000000};
    nanosleep(&t, 0);
    kill(vchild, SIGKILL);
    return 0;
}

static int vfork_killed(int spin) {
    pthread_t th;
    vchild = 0;
    pthread_create(&th, 0, killer, 0);
    pid_t pid = vfork();
    if (pid == 0) {
        vchild = getpid();
        if (spin) for (;;) {}
        struct timespec t = {30, 0};
        nanosleep(&t, 0);
        _exit(3);
    }
    int st = 0;
    waitpid(pid, &st, 0);
    pthread_join(th, 0);
    return WIFSIGNALED(st) ? WTERMSIG(st) : -1;
}

static void zombies(void) {
    int made = 0, err = 0;
    for (int k = 0; k < 200; k++) {
        pid_t pid = fork();
        if (pid == 0) _exit(0);
        if (pid < 0) { err = errno; break; }
        made++;
        /* It has ended (its app slot is free again) but stays a zombie. */
        siginfo_t si;
        waitid(P_PID, pid, &si, WEXITED | WNOWAIT);
    }
    int reaped = 0;
    while (waitpid(-1, 0, 0) > 0) reaped++;
    pid_t again = fork();
    if (again == 0) _exit(0);
    waitpid(again, 0, 0);
    printf("charden: zombies capped at %d EAGAIN %d reaped %d, then fork %d\n", made, err == EAGAIN, reaped == made ? reaped : -reaped, again > 0);
}

static void spawn_loop(void) {
    int ok = 0;
    for (int k = 0; k < 400; k++) {
        pid_t pid = vfork();
        if (pid == 0) _exit(1);
        int st = 0;
        if (pid > 0 && waitpid(pid, &st, 0) == pid && WEXITSTATUS(st) == 1) ok++;
    }
    printf("charden: 400 vforks %d\n", ok == 400);
}

static int exec_errno(const char *path) {
    char *argv[] = {(char *)path, 0};
    execv(path, argv);
    return errno;
}
static void write_file(const char *path, const void *data, size_t n) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0755);
    write(fd, data, n);
    close(fd);
}

static void exec_checks(void) {
    /* A "#!" naming a hidden runtime file and one naming nothing: the same answer. */
    write_file("/tmp/charden-a", "#!/data/.grants\n", 16);
    write_file("/tmp/charden-b", "#!/data/.charden-none\n", 22);
    int a = exec_errno("/tmp/charden-a"), b = exec_errno("/tmp/charden-b");
    printf("charden: hidden interpreter %s, missing %s\n", a == ENOENT ? "ENOENT" : strerror(a), b == ENOENT ? "ENOENT" : strerror(b));
    /* A static ELF whose code shares a 64 KiB page with its data: the headers
     * pass, the loader would refuse it. The execve fails; the caller lives. */
    uint32_t elf[13 + 16] = {0};
    unsigned char *h = (unsigned char *)elf;
    memcpy(h, "\x7f" "ELF\x01\x01\x01", 7);
    h[16] = 2; h[18] = 40; h[20] = 1;  /* ET_EXEC, EM_ARM, EV_CURRENT */
    elf[6] = 0x10000;                   /* e_entry */
    elf[7] = 52;                        /* e_phoff */
    h[40] = 52; h[42] = 32; h[44] = 2;  /* e_ehsize, e_phentsize, e_phnum */
    uint32_t text[8] = {1, 0, 0x10000, 0x10000, 0x40, 0x40, 5, 0x1000};
    uint32_t data[8] = {1, 0x40, 0x18040, 0x18040, 0x10, 0x100, 6, 0x1000};
    memcpy(elf + 13, text, 32);
    memcpy(elf + 21, data, 32);
    write_file("/tmp/charden-mixed", elf, sizeof elf);
    pid_t pid = fork();
    if (pid == 0) _exit(exec_errno("/tmp/charden-mixed") == ENOEXEC ? 42 : 1);
    int st = 0;
    waitpid(pid, &st, 0);
    printf("charden: refused image ENOEXEC, caller lived %d\n", WIFEXITED(st) && WEXITSTATUS(st) == 42);
}

int main(void) {
    vfp_fork();
    timer_floor();
    sigbus();
    printf("charden: vfork child killed asleep %d spinning %d\n", vfork_killed(0), vfork_killed(1));
    zombies();
    spawn_loop();
    exec_checks();
    printf("charden done\n");
    return 0;
}
