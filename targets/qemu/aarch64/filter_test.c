// The handler reads real seccomp SIGSYS information and changes the return
// register in the guest signal frame, as Ladybird's SIGSYS broker does.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/ucontext.h>
#include <sys/wait.h>
#include <unistd.h>

struct ins { uint16_t op; uint8_t yes, no; uint32_t k; };
struct filter { uint16_t size; struct ins *code; };
static volatile sig_atomic_t trapped;
static void catch_call(int signum, siginfo_t *info, void *raw) {
    ucontext_t *ctx = raw;
    trapped = signum == SIGSYS && info->si_code == 1 && info->si_errno == 81 &&
              info->si_syscall == SYS_getppid && info->si_arch == 0xc00000b7;
    ctx->uc_mcontext.regs[0] = 1234;
}
int main(void) {
    struct sigaction handler = {.sa_sigaction=catch_call, .sa_flags=SA_SIGINFO};
    if (sigaction(SIGSYS, &handler, 0)) return 1;
    struct ins code[] = {
        {0x20,0,0,0}, {0x15,0,1,SYS_getpid}, {6,0,0,0x50000 | EACCES},
        {0x15,0,1,SYS_getppid}, {6,0,0,0x30000 | 81}, {6,0,0,0x7fff0000},
    };
    struct filter f = {sizeof(code)/sizeof(*code), code};
    if (syscall(SYS_seccomp,1,0,&f) != -1 || errno != EACCES) return 2;
    if (prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0) || syscall(SYS_seccomp,1,1,&f)) return 3;
    if (syscall(SYS_getpid) != -1 || errno != EACCES) return 4;
    if (syscall(SYS_getppid) != 1234 || !trapped) return 5;
    int child = fork();
    if (!child) _exit(syscall(SYS_getpid) == -1 && errno == EACCES ? 0 : 1);
    int status;
    if (child < 0 || waitpid(child,&status,0) != child || status) return 6;
    puts("VioletOS seccomp, SIGSYS context and inheritance passed");
    return 0;
}
