// Check groups inherited at fork, group-targeted kill, and waitpid by group.
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int sync[2], ready;
    if (pipe(sync)) return 1;
    int child = fork();
    if (child < 0) return 2;
    if (!child) {
        int ok = getpgrp() == getppid() && !setpgid(0, 0) && getpgrp() == getpid();
        if (write(sync[1], &ok, sizeof(ok)) != sizeof(ok)) _exit(1);
        for (;;) { }
    }
    if (read(sync[0], &ready, sizeof(ready)) != sizeof(ready) || !ready) return 3;
    if (getpgid(child) != child || kill(-child, 0) || kill(-child, SIGTERM)) return 4;
    int status;
    if (waitpid(-child, &status, 0) != child || !WIFSIGNALED(status) || WTERMSIG(status) != SIGTERM) return 5;
    puts("VioletOS process groups and signal termination passed");
    return 0;
}
