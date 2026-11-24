/* A static C program against musl: stdio, malloc, pthreads, time and files. */
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <math.h>

static void *work(void *arg) { long n = (long)arg; long s = 0; for (long i = 1; i <= n; i++) s += i; return (void *)s; }

int main(void) {
    printf("hello from c on violet\n");
    pthread_t t[4];
    long total = 0;
    for (long i = 0; i < 4; i++) pthread_create(&t[i], 0, work, (void *)(1000 * (i + 1)));
    for (int i = 0; i < 4; i++) { void *r; pthread_join(t[i], &r); total += (long)r; }
    printf("pthreads summed %ld\n", total);
    double x = sqrt(2.0) * sin(1.0);
    printf("libm: %.6f\n", x);
    struct timespec a, b; clock_gettime(CLOCK_MONOTONIC, &a);
    struct timespec d = {0, 20000000}; nanosleep(&d, 0);
    clock_gettime(CLOCK_MONOTONIC, &b);
    long ms = (b.tv_sec - a.tv_sec) * 1000 + (b.tv_nsec - a.tv_nsec) / 1000000;
    printf("slept at least 20 ms: %s\n", ms >= 20 ? "yes" : "no");
    FILE *f = fopen("/tmp/c.txt", "w"); fputs("written by c", f); fclose(f);
    char buf[64] = {0}; f = fopen("/tmp/c.txt", "r"); fgets(buf, sizeof buf, f); fclose(f);
    printf("file round trip: %s\n", buf);
    char *big = malloc(16 << 20); memset(big, 3, 16 << 20); printf("16 MiB malloc ok: %d\n", big[12345] == 3); free(big);
    printf("c done\n");
    return 0;
}
