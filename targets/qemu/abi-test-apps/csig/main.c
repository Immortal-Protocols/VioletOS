/* Linux signals under the Open Runtime (make test-abi): handlers with siginfo
 * on an alternate stack, masks and pending signals, SIGSEGV and SIGILL taken by
 * handlers, a thread interrupted while it runs in user code (its VFP registers
 * and, in Thumb code, its IT and GE state kept), SA_RESTART against EINTR (in
 * ARM and in Thumb code), alarm, interval and POSIX timers, sigtimedwait. */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

static volatile sig_atomic_t usr1, usr2, alrm, timer_hits, info_ok, on_alt;
static volatile void *fault_addr;
static volatile int fault_code;
static sigjmp_buf jump;
static char altstack[16384];

static void on_usr1(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    usr1++;
    if (sig == SIGUSR1 && si->si_signo == SIGUSR1 && si->si_pid == getpid()) info_ok = 1;
    char here;
    on_alt = &here >= altstack && &here < altstack + sizeof altstack;
}
static void on_usr2(int sig) {
    /* Uses the VFP registers the interrupted code had live. */
    volatile double x = sig;
    for (int i = 0; i < 64; i++) x = x * 1.5 + 0.25;
    usr2++;
}
static void on_segv(int sig, siginfo_t *si, void *uc) { (void)sig; (void)uc; fault_addr = si->si_addr; fault_code = si->si_code; siglongjmp(jump, 1); }
static void on_ill(int sig) { (void)sig; siglongjmp(jump, 2); }
static void on_alrm(int sig) { (void)sig; alrm++; }
static void on_timer(int sig, siginfo_t *si, void *uc) { (void)sig; (void)uc; if (si->si_code == SI_TIMER) timer_hits++; }

/* Spins with no system call until a handler has run: only asynchronous delivery reaches it. */
static void *spinner(void *arg) {
    double acc = 0.5;
    while (!usr2) acc = acc * 1.0000001 + 1e-9;
    *(double *)arg = acc;
    return 0;
}

/* Thumb code with IT blocks and the GE flags (sadd8, then sel) live while signals arrive. */
static volatile int stop;
__attribute__((target("thumb"), noinline)) static unsigned thumb_step(unsigned a) {
    unsigned b, g;
    __asm__ volatile("cmp %1, #0x80\n\tite lo\n\taddlo %0, %1, #3\n\tsubhs %0, %1, #1" : "=&r"(b) : "r"(a & 0xff) : "cc");
    __asm__ volatile("sadd8 %0, %1, %2\n\tsel %0, %1, %2" : "=&r"(g) : "r"(a), "r"(0x80808080u));
    return b ^ g;
}
static unsigned reference_step(unsigned a) {
    unsigned lo = a & 0xff, b = lo < 0x80 ? lo + 3 : lo - 1, g = 0;
    for (int k = 0; k < 4; k++) {
        int lane = (signed char)(a >> (8 * k)) + (signed char)0x80;
        g |= (lane >= 0 ? (a >> (8 * k)) & 0xff : 0x80) << (8 * k);
    }
    return b ^ g;
}
static unsigned thumb_iterations, thumb_sum;
static void *thumb_worker(void *arg) {
    (void)arg;
    unsigned sum = 0, i = 0;
    while (!stop) sum += thumb_step(i++ * 2654435761u);
    thumb_iterations = i;
    thumb_sum = sum;
    return 0;
}

/* read(2) through an svc in Thumb code, for the restart rule there. */
__attribute__((target("thumb"), noinline)) static long thumb_read(int fd, void *buf, long n) {
    register long r0 __asm__("r0") = fd;
    register void *r1 __asm__("r1") = buf;
    register long r2 __asm__("r2") = n;
    __asm__ volatile("push {r7}\n\tmovs r7, #3\n\tsvc 0\n\tpop {r7}" : "+r"(r0) : "r"(r1), "r"(r2) : "memory");
    return r0;
}

static int pfd[2];
static volatile long got;
static volatile int got_errno, use_thumb;
static void *reader(void *arg) {
    (void)arg;
    char c;
    got = use_thumb ? thumb_read(pfd[0], &c, 1) : read(pfd[0], &c, 1);
    if (!use_thumb) got_errno = errno; else if (got < 0) got_errno = -got, got = -1;
    return 0;
}
static void interrupted_read(int restart, int thumb) {
    struct sigaction sa = {0};
    sa.sa_handler = on_usr2;
    sa.sa_flags = restart ? SA_RESTART : 0;
    sigaction(SIGUSR2, &sa, 0);
    use_thumb = thumb;
    got_errno = 0;
    pipe(pfd);
    pthread_t t;
    pthread_create(&t, 0, reader, 0);
    usleep(30000);
    pthread_kill(t, SIGUSR2);
    usleep(30000);
    write(pfd[1], "x", 1);
    pthread_join(t, 0);
    close(pfd[0]);
    close(pfd[1]);
}

__attribute__((noinline)) static void poke(volatile int *p) { *p = 6; }

int main(void) {
    struct sigaction sa = {0};
    stack_t ss = {.ss_sp = altstack, .ss_size = sizeof altstack};
    sigaltstack(&ss, 0);
    sa.sa_sigaction = on_usr1;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigaction(SIGUSR1, &sa, 0);
    raise(SIGUSR1);
    kill(getpid(), SIGUSR1);
    printf("csig: usr1 %d info %d altstack %d\n", usr1, info_ok, on_alt);

    sigset_t set, old, pend;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    sigprocmask(SIG_BLOCK, &set, &old);
    raise(SIGUSR1);
    sigpending(&pend);
    int before = usr1, pending = sigismember(&pend, SIGUSR1);
    sigprocmask(SIG_SETMASK, &old, 0);
    printf("csig: blocked %d pending %d unblocked %d\n", before, pending, usr1);

    sa.sa_sigaction = on_segv;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, 0);
    if (!sigsetjmp(jump, 1)) *(volatile int *)16 = 1;
    printf("csig: SIGSEGV at %p code %d\n", fault_addr, fault_code);
    /* A constant: the page is read-only, so the write faults with SEGV_ACCERR. */
    if (!sigsetjmp(jump, 1)) poke((volatile int *)&"constant"[0]);
    printf("csig: write to a constant code %d\n", fault_code);

    signal(SIGILL, on_ill);
    int ill = sigsetjmp(jump, 1);
    if (!ill) __asm__ volatile(".inst 0xe7f000f0");
    printf("csig: SIGILL %d\n", ill == 2);

    double result = 0;
    pthread_t t;
    signal(SIGUSR2, on_usr2);
    pthread_create(&t, 0, spinner, &result);
    usleep(30000);
    pthread_kill(t, SIGUSR2);
    pthread_join(t, 0);
    printf("csig: spinner interrupted %d vfp %s\n", usr2 > 0, result > 0.5 && result < 2.0 ? "kept" : "lost");

    sa.sa_sigaction = on_usr1;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGUSR1, &sa, 0);
    pthread_create(&t, 0, thumb_worker, 0);
    int sent = 0;
    for (; sent < 2000; sent++) {
        pthread_kill(t, SIGUSR1);
        if (sent % 16 == 0) usleep(200);
    }
    usleep(20000);
    stop = 1;
    pthread_join(t, 0);
    unsigned expect = 0;
    for (unsigned i = 0; i < thumb_iterations; i++) expect += reference_step(i * 2654435761u);
    printf("csig: thumb IT/GE under %d signals %s\n", sent, expect == thumb_sum && thumb_iterations ? "kept" : "lost");

    interrupted_read(1, 0);
    long restarted = got;
    interrupted_read(0, 0);
    printf("csig: SA_RESTART read %ld, without %ld %s\n", restarted, got, got_errno == EINTR ? "EINTR" : "?");
    interrupted_read(1, 1);
    restarted = got;
    interrupted_read(0, 1);
    printf("csig: thumb svc SA_RESTART read %ld, without %ld %s\n", restarted, got, got_errno == EINTR ? "EINTR" : "?");

    signal(SIGALRM, on_alrm);
    alarm(1);
    pause();
    printf("csig: alarm %d\n", alrm);
    struct itimerval it = {{0, 20000}, {0, 20000}};
    alrm = 0;
    setitimer(ITIMER_REAL, &it, 0);
    while (alrm < 5) pause();
    memset(&it, 0, sizeof it);
    setitimer(ITIMER_REAL, &it, 0);
    printf("csig: interval timer %d\n", alrm >= 5);

    timer_t tm;
    struct sigevent ev = {.sigev_notify = SIGEV_SIGNAL, .sigev_signo = SIGRTMIN + 1};
    sa.sa_sigaction = on_timer;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGRTMIN + 1, &sa, 0);
    timer_create(CLOCK_MONOTONIC, &ev, &tm);
    struct itimerspec its = {{0, 10000000}, {0, 10000000}};
    timer_settime(tm, 0, &its, 0);
    while (timer_hits < 3) pause();
    timer_delete(tm);
    printf("csig: posix timer %d\n", timer_hits >= 3);

    sigprocmask(SIG_BLOCK, &set, 0);
    kill(getpid(), SIGUSR1);
    siginfo_t si;
    int s = sigtimedwait(&set, &si, &(struct timespec){1, 0});
    int to = sigtimedwait(&set, &si, &(struct timespec){0, 10000000});
    printf("csig: sigtimedwait %d then %d %s\n", s, to, errno == EAGAIN ? "EAGAIN" : "?");
    printf("csig done\n");
    return 0;
}
