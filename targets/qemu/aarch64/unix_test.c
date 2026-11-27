#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>
#define REQUIRE(x)                                                                                                     \
    do {                                                                                                               \
        if (!(x)) {                                                                                                    \
            printf("line %d errno %d\n", __LINE__, errno);                                                             \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)
static int message_fd(int transport, int descriptor, int flags, int *received) {
    char token = 'x', control[CMSG_SPACE(sizeof(int))] = {0};
    struct iovec iov = {&token, 1};
    struct msghdr msg = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof(control)};
    struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
    if (descriptor >= 0) {
        c->cmsg_level = SOL_SOCKET;
        c->cmsg_type = SCM_RIGHTS;
        c->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(c), &descriptor, sizeof(descriptor));
        return sendmsg(transport, &msg, flags);
    }
    int n = recvmsg(transport, &msg, flags);
    if (n > 0 && (c = CMSG_FIRSTHDR(&msg)) && c->cmsg_len == CMSG_LEN(sizeof(int)))
        memcpy(received, CMSG_DATA(c), sizeof(*received));
    return n;
}
static int descriptor_lifetime(void) {
    int a[2], b[2], fd = -1, peek = -1;
    char token;
    REQUIRE(socketpair(AF_UNIX, SOCK_STREAM, 0, a) == 0 && socketpair(AF_UNIX, SOCK_STREAM, 0, b) == 0);
    REQUIRE(message_fd(a[0], b[0], 0, NULL) == 1);
    REQUIRE(close(b[0]) == 0);
    REQUIRE(message_fd(a[1], -1, MSG_PEEK, &peek) == 1 && peek >= 0);
    REQUIRE(message_fd(a[1], -1, MSG_CMSG_CLOEXEC, &fd) == 1 && fd >= 0 && fd != peek);
    REQUIRE(close(peek) == 0 && write(fd, "x", 1) == 1 && read(b[1], &token, 1) == 1 && token == 'x');
    REQUIRE(close(fd) == 0 && read(b[1], &token, 1) == 0);
    REQUIRE(close(b[1]) == 0);
    // Socket a[1] is held only by a descriptor in its own receive queue.
    REQUIRE(message_fd(a[0], a[1], 0, NULL) == 1 && close(a[1]) == 0);
    REQUIRE(recv(a[0], &token, 1, MSG_DONTWAIT) == 0 && close(a[0]) == 0);
    // Two receive queues with no external reference to either receiving endpoint.
    REQUIRE(socketpair(AF_UNIX, SOCK_STREAM, 0, a) == 0 && socketpair(AF_UNIX, SOCK_STREAM, 0, b) == 0);
    REQUIRE(message_fd(a[0], b[1], 0, NULL) == 1 && message_fd(b[0], a[1], 0, NULL) == 1);
    REQUIRE(close(a[1]) == 0 && close(b[1]) == 0);
    REQUIRE(recv(a[0], &token, 1, MSG_DONTWAIT) == 0 && recv(b[0], &token, 1, MSG_DONTWAIT) == 0);
    REQUIRE(close(a[0]) == 0 && close(b[0]) == 0);
    puts("SCM_RIGHTS queued references, MSG_PEEK and socket-cycle collection passed");
    return 0;
}
static int fork_simd(void) {
    long pid;
    unsigned long pattern;
    // Keep a nonzero lane live across the Linux clone syscall.
    __asm__ volatile(
        "movi v31.16b, #90\n"
        "mov x0, #17\nmov x1, #0\nmov x2, #0\nmov x3, #0\nmov x4, #0\n"
        "mov x8, #220\nsvc #0\n"
        "mov %0, x0\numov %1, v31.d[0]"
        : "=&r"(pid), "=&r"(pattern) : : "x0","x1","x2","x3","x4","x8","v31","memory");
    if (pid == 0)
        _exit(pattern == 0x5a5a5a5a5a5a5a5aUL ? 0 : 1);
    int status;
    REQUIRE(pid > 0 && waitpid(pid, &status, 0) == pid && status == 0 && pattern == 0x5a5a5a5a5a5a5a5aUL);
    puts("AArch64 child inherited parent FP/SIMD state across fork");
    return 0;
}
static int many_pairs(void) {
    enum { pairs = 160, rounds = 3 };
    int sockets[pairs][2];
    for (int round = 0; round < rounds; ++round) {
        for (int i = 0; i < pairs; ++i) {
            REQUIRE(socketpair(AF_UNIX, SOCK_STREAM, 0, sockets[i]) == 0);
            REQUIRE(write(sockets[i][0], &i, sizeof(i)) == sizeof(i));
        }
        for (int i = pairs - 1; i >= 0; --i) {
            int token;
            REQUIRE(read(sockets[i][1], &token, sizeof(token)) == sizeof(token) && token == i);
            REQUIRE(close(sockets[i][1]) == 0);
            REQUIRE(read(sockets[i][0], &token, sizeof(token)) == 0);
            REQUIRE(close(sockets[i][0]) == 0);
        }
    }
    puts("320 simultaneous AF_UNIX endpoints, independent streams, EOF and reuse passed");
    return 0;
}
int main(void) {
    REQUIRE(many_pairs() == 0 && descriptor_lifetime() == 0 && fork_simd() == 0);
    fflush(stdout);
    int pair[2];
    REQUIRE(socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, pair) == 0);
    int m = memfd_create("frame", MFD_CLOEXEC | MFD_ALLOW_SEALING);
    REQUIRE(m >= 0 && !ftruncate(m, 8192));
    REQUIRE(fcntl(m, F_ADD_SEALS, F_SEAL_SHRINK | F_SEAL_GROW) == 0);
    REQUIRE(ftruncate(m, 1) < 0 && errno == EPERM);
    char *mapped = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, m, 0);
    REQUIRE(mapped != MAP_FAILED);
    strcpy(mapped + 4093, "shared-by-parent");
    int pid = fork();
    REQUIRE(pid >= 0);
    char control[CMSG_SPACE(sizeof(int))] = {0}, data[8] = "payload";
    struct iovec iov[2] = {{data, 2}, {data + 2, 5}};
    struct msghdr msg = {.msg_iov = iov, .msg_iovlen = 2, .msg_control = control, .msg_controllen = sizeof(control)};
    if (!pid) {
        close(pair[0]);
        close(m);
        REQUIRE(!munmap(mapped, 8192));
        struct pollfd pfd = {pair[1], POLLIN, 0};
        REQUIRE(poll(&pfd, 1, 5000) == 1 && (pfd.revents & POLLIN));
        memset(data, 0, sizeof(data));
        REQUIRE(recvmsg(pair[1], &msg, MSG_CMSG_CLOEXEC) == 7 && !strcmp(data, "payload"));
        struct cmsghdr *h = CMSG_FIRSTHDR(&msg);
        REQUIRE(h && h->cmsg_type == SCM_RIGHTS && h->cmsg_len == CMSG_LEN(sizeof(int)));
        memcpy(&m, CMSG_DATA(h), sizeof(m));
        REQUIRE(m >= 0 && (fcntl(m, F_GETFD) & FD_CLOEXEC));
        char *remote = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, m, 0);
        REQUIRE(remote != MAP_FAILED && !strcmp(remote + 4093, "shared-by-parent"));
        strcpy(remote + 4093, "changed-by-child");
        REQUIRE(write(pair[1], "ok", 2) == 2);
        return 0;
    }
    close(pair[1]);
    struct cmsghdr *h = CMSG_FIRSTHDR(&msg);
    h->cmsg_level = SOL_SOCKET;
    h->cmsg_type = SCM_RIGHTS;
    h->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(h), &m, sizeof(m));
    REQUIRE(sendmsg(pair[0], &msg, 0) == 7);
    close(m);
    REQUIRE(read(pair[0], data, 2) == 2 && !memcmp(data, "ok", 2));
    int status;
    REQUIRE(waitpid(pid, &status, 0) == pid && status == 0 && !strcmp(mapped + 4093, "changed-by-child"));
    struct sockaddr_un addr = {.sun_family = AF_UNIX, .sun_path = "/tmp/test.sock"};
    int server = socket(AF_UNIX, SOCK_STREAM, 0), client = socket(AF_UNIX, SOCK_STREAM, 0);
    REQUIRE(server >= 0 && client >= 0 && bind(server, (void *)&addr, sizeof(addr)) == 0 && listen(server, 3) == 0);
    REQUIRE(connect(client, (void *)&addr, sizeof(addr)) == 0);
    int con = accept4(server, NULL, NULL, SOCK_NONBLOCK);
    REQUIRE(con >= 0 && read(con, data, 1) < 0 && errno == EAGAIN);
    REQUIRE(write(client, "abc", 3) == 3 && read(con, data, 3) == 3 && !memcmp(data, "abc", 3));
    close(server);
    close(client);
    close(con);
    REQUIRE(!unlink(addr.sun_path));
    puts("AArch64 Unix socket: SCM_RIGHTS, shared memfd, file seals and named connect passed");
    return 0;
}
