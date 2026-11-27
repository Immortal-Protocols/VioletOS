// Read well beyond the initial boot seed, using both Linux entropy interfaces.
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/random.h>
#include <unistd.h>
int main(void) {
    unsigned char a[4096], b[4096];
    int fd = open("/dev/urandom", O_RDONLY);
    if (fd < 0) return 1;
    for (int i=0; i<12; i++) {
        for (size_t n=0; n<sizeof(a);) {
            ssize_t r = getrandom(a+n, sizeof(a)-n, 0);
            if (r <= 0) return 2;
            n += r;
        }
        for (size_t n=0; n<sizeof(b);) {
            ssize_t r = read(fd, b+n, sizeof(b)-n);
            if (r <= 0) return 3;
            n += r;
        }
        if (!memcmp(a,b,sizeof(a))) return 4;
    }
    puts("VioletOS received fresh entropy across 96 KiB of reads");
    return 0;
}
