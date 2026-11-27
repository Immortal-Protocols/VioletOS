// Packed code and data can be lent read-only. A private writable mapping and
// its forked child must still see independent copies after changing protection.
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int fd = open("/bin/packed-test", O_RDONLY);
    if (fd < 0) return 1;
    uint32_t *p = mmap(NULL, 8192, PROT_READ, MAP_PRIVATE, fd, 0);
    uint32_t *q = mmap(NULL, 8192, PROT_READ, MAP_SHARED, fd, 0);
    if (p == MAP_FAILED || q == MAP_FAILED || *p != 0x464c457f || *q != *p) return 2;
    if (mprotect(p, 4096, PROT_READ | PROT_WRITE)) return 3;
    *p = 42;
    if (*q != 0x464c457f || mprotect(p, 4096, PROT_READ)) return 4;
    pid_t child = fork();
    if (child < 0) return 5;
    if (!child) {
        if (*p != 42 || *q != 0x464c457f || mprotect(p, 4096, PROT_READ | PROT_WRITE)) _exit(6);
        *p = 99;
        _exit(*q == 0x464c457f ? 0 : 7);
    }
    int status = -1;
    if (waitpid(child, &status, 0) != child || status || *p != 42) return 8;
    if (madvise(p, 4096, MADV_DONTNEED) || *p != *q) return 9;
    puts("Immutable file pages and private writes survived fork and discard");
    return 0;
}
