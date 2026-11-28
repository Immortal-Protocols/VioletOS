/* Processes under the Open Runtime (make test-system): fork (a copy of memory),
 * fork then execve, posix_spawn (clone with CLONE_VM|CLONE_VFORK) with a pipe,
 * an environment and a close-on-exec descriptor, a missing program, vfork,
 * a child killed by a signal, SIGCHLD, ECHILD, and the kill rule. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile sig_atomic_t chld;
static void on_chld(int sig) { (void)sig; chld++; }
static int counter = 1;

int main(void) {
    signal(SIGCHLD, on_chld);
    int *heap = malloc(sizeof *heap), st = 0;
    *heap = 10;
    pid_t pid = fork();
    if (pid == 0) {
        counter = 2;
        *heap = 20;
        _exit(counter + *heap);
    }
    pid_t w = waitpid(pid, &st, 0);
    printf("cproc: fork %d exit %d, parent kept %d %d\n", w == pid, WIFEXITED(st) ? WEXITSTATUS(st) : -1, counter, *heap);

    pid = fork();
    if (pid == 0) {
        char *a[] = {"/apps/abichild", "exec", 0};
        execv(a[0], a);
        _exit(99);
    }
    waitpid(pid, &st, 0);
    printf("cproc: fork+exec status %d\n", WEXITSTATUS(st));

    int p[2];
    pipe(p);
    int secret = open("/apps/abichild", O_RDONLY | O_CLOEXEC);
    char fd[16], said[96] = {0};
    snprintf(fd, sizeof fd, "%d", secret);
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, p[1], 1);
    char *argv[] = {"/apps/abichild", "spawn", fd, 0}, *env[] = {"ABI=spawned", 0};
    int r = posix_spawn(&pid, argv[0], &fa, 0, argv, env);
    close(p[1]);
    for (size_t n = 0; n < sizeof said - 1;) {
        ssize_t k = read(p[0], said + n, sizeof said - 1 - n);
        if (k <= 0) break;
        n += k;
    }
    waitpid(pid, &st, 0);
    printf("cproc: spawn %d said '%s' status %d\n", r, said, WEXITSTATUS(st));

    r = posix_spawn(&pid, "/apps/no-such-program", 0, 0, argv, env);
    printf("cproc: missing program %s\n", r == ENOENT ? "ENOENT" : strerror(r));

    pid = vfork();
    if (pid == 0) _exit(5);
    waitpid(pid, &st, 0);
    printf("cproc: vfork status %d\n", WEXITSTATUS(st));

    pid = fork();
    if (pid == 0) for (;;) pause();
    kill(pid, SIGTERM);
    waitpid(pid, &st, 0);
    printf("cproc: killed child signal %d\n", WIFSIGNALED(st) ? WTERMSIG(st) : 0);

    char *kp[] = {"/apps/abichild", "kill-parent", 0};
    posix_spawn(&pid, kp[0], 0, 0, kp, env);
    waitpid(pid, &st, 0);
    printf("cproc: child may not signal its parent %d\n", WEXITSTATUS(st) == 0);
    int none = waitpid(-1, &st, WNOHANG) == -1 && errno == ECHILD;
    printf("cproc: then %s, SIGCHLD %d\n", none ? "ECHILD" : "?", chld >= 4);
    printf("cproc done\n");
    return 0;
}
