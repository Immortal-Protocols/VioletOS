// Verify lock conflict, split, read sharing, and close semantics across fork.
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

static void require(int ok) { if (!ok) { perror("record lock check"); _exit(1); } }
static int lock(int fd, int cmd, int type, int start, int len) {
    struct flock range = {.l_type=type, .l_whence=SEEK_SET, .l_start=start, .l_len=len};
    return fcntl(fd, cmd, &range);
}
int main(void) {
    int fd = open("/tmp/violet-locks", O_RDWR | O_CREAT, 0600);
    require(fd >= 0);
    require(lock(fd, F_SETLK, F_WRLCK, 2, 0) == 0);
    require(lock(fd, F_SETLK, F_UNLCK, 4, 3) == 0);
    int sync[2]; require(pipe(sync) == 0);
    int child = fork(); require(child >= 0);
    if (child == 0) {
        require(lock(fd, F_SETLK, F_RDLCK, 2, 1) < 0 && errno == EAGAIN);
        struct flock r = {.l_type=F_WRLCK, .l_whence=SEEK_SET, .l_start=8, .l_len=1};
        require(fcntl(fd, F_GETLK, &r) == 0 && r.l_type == F_WRLCK && r.l_pid == getppid()
                && r.l_start == 7 && r.l_len == 0);
        require(lock(fd, F_SETLK, F_RDLCK, 4, 2) == 0);
        require(write(sync[1], "x", 1) == 1);
        require(lock(fd, F_SETLKW, F_WRLCK, 0, 0) == 0);
        _exit(0);
    }
    char c;
    ssize_t n;
    do { n = read(sync[0], &c, 1); } while (n < 0 && errno == EAGAIN);
    require(n == 1 && c == 'x');
    require(lock(fd, F_SETLK, F_RDLCK, 4, 2) == 0);
    require(lock(fd, F_SETLK, F_WRLCK, 4, 2) == -1 && errno == EAGAIN);
    int other = dup(fd); require(other >= 0 && close(other) == 0);
    int status; require(waitpid(child, &status, 0) == child && status == 0);
    puts("VioletOS cross-process file locks passed");
    return 0;
}
