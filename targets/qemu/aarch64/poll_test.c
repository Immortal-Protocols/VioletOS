#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

static int wait_event(int fd, int events) {
    // Most entries are ignored. The Linux limit is RLIMIT_NOFILE.
    struct pollfd set[129];
    for (unsigned i = 0; i < 128; ++i) set[i] = (struct pollfd){.fd = -1};
    set[128] = (struct pollfd){.fd = fd, .events = events};
    return poll(set, 129, -1) == 1 ? set[128].revents : -1;
}
int main(void) {
    int pair[2], pipefd[2];
    int event = eventfd(0, EFD_CLOEXEC);
    if (event < 0 || socketpair(AF_UNIX, SOCK_STREAM, 0, pair) || pipe(pipefd)) return 1;
    int pid = fork();
    if (pid < 0) return 2;
    if (!pid) {
        close(pair[0]); close(pipefd[0]);
        usleep(20000);
        if (write(pair[1], "a", 1) != 1) _exit(3);
        usleep(20000);
        if (write(pipefd[1], "b", 1) != 1) _exit(4);
        usleep(20000);
        if (eventfd_write(event, 7)) _exit(5);
        usleep(20000);
        close(pair[1]); close(pipefd[1]); _exit(0);
    }
    close(pair[1]); close(pipefd[1]);
    char byte;
    eventfd_t count;
    if (wait_event(pair[0], POLLIN) != POLLIN || read(pair[0], &byte, 1) != 1 || byte != 'a') return 6;
    if (wait_event(pipefd[0], POLLIN) != POLLIN || read(pipefd[0], &byte, 1) != 1 || byte != 'b') return 7;
    if (wait_event(event, POLLIN) != POLLIN || eventfd_read(event, &count) || count != 7) return 8;
    if (!(wait_event(pair[0], POLLIN) & POLLHUP) || !(wait_event(pipefd[0], POLLIN) & POLLHUP)) return 9;
    struct pollfd idle = {.fd = event, .events = POLLIN};
    if (poll(&idle, 1, 15) != 0) return 10;
    int status;
    if (waitpid(pid, &status, 0) != pid || status) return 11;
    puts("AArch64 ppoll: 129 slots, Unix streams, pipes, eventfd, EOF and timeout passed");
    return 0;
}
