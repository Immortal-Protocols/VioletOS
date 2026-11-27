// Build against AArch64 musl to exercise its real posix_spawn/exec/wait path.
#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;
static volatile int marker = 42;

static int wait_code(pid_t pid) {
    int status;
    pid_t done;
    do done = waitpid(pid, &status, 0); while (done == -1 && errno == EINTR);
    return done == pid && WIFEXITED(status) ? WEXITSTATUS(status) : 255;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--child") == 0)
        return getenv("VIOLET_SPAWN") && write(3, "child", 5) == 5 ? 23 : 111;
    if (argc > 1) {
        pid_t pid;
        int err = posix_spawn(&pid, argv[1], NULL, NULL, argv + 1, environ);
        if (err) { fprintf(stderr, "posix_spawn: %s\n", strerror(err)); return 1; }
        int status = wait_code(pid);
        printf("VioletOS parent waited for %s (status %d)\n", argv[1], status);
        return status;
    }
    pid_t pid = fork();
    if (!pid) { marker = 0; _Exit(17); }
    if (pid < 0 || wait_code(pid) != 17 || marker != 42) return 2;
    for (int i = 0; i < 12; i++) {
        int fds[2];
        if (pipe2(fds, O_CLOEXEC)) return 3;
        posix_spawn_file_actions_t actions;
        if (posix_spawn_file_actions_init(&actions) ||
            posix_spawn_file_actions_adddup2(&actions, fds[1], 3)) return 4;
        char *args[] = {argv[0], "--child", NULL}, *env[] = {"VIOLET_SPAWN=1", NULL};
        int rc = posix_spawn(&pid, argv[0], &actions, NULL, args, env);
        posix_spawn_file_actions_destroy(&actions);
        if (rc) { fprintf(stderr, "posix_spawn: %s\n", strerror(rc)); return 5; }
        close(fds[1]);
        if (wait_code(pid) != 23) return 6;
        char text[6] = {0};
        if (read(fds[0], text, 5) != 5 || strcmp(text, "child")) return 7;
        close(fds[0]);
    }
    puts("VioletOS fork isolation, posix_spawn, inherited pipe and waitpid passed");
    return 0;
}
