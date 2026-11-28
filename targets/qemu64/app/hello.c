// The C test program of the qemu64 target: an ordinary program linked
// statically against musl for AArch64. Nothing in it knows about VioletOS;
// it checks that the C library starts, finds its thread-local storage,
// allocates and prints on the 64-bit bring-up runtime.
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static __thread int thread_local = 41;
static __thread char thread_zero[64];

int main(int argc, char **argv) {
  int ok = 1;
  printf("hello from musl on AArch64: %s, %d argument%s\n", argv[0], argc - 1, argc == 2 ? "" : "s");
  // Thread-local storage: musl copies the program's TLS template (found
  // through AT_PHDR) and points TPIDR_EL0 at it itself, with no system call.
  thread_local++;
  ok &= thread_local == 42 && thread_zero[63] == 0;
  printf("thread-local storage: %s\n", thread_local == 42 ? "ok" : "FAILED");
  // The allocator: small blocks from brk, a large one from mmap.
  char *small = malloc(100), *large = malloc(1 << 20);
  int heap = small && large;
  if (heap) {
    memset(large, 0x5a, 1 << 20);
    strcpy(small, "heap");
    heap = large[(1 << 20) - 1] == 0x5a && !strcmp(small, "heap");
    free(large);
    free(small);
  }
  ok &= heap;
  printf("malloc and free: %s\n", heap ? "ok" : "FAILED");
  struct timespec a, b;
  int clock = !clock_gettime(CLOCK_MONOTONIC, &a) && !clock_gettime(CLOCK_MONOTONIC, &b) &&
              (b.tv_sec > a.tv_sec || (b.tv_sec == a.tv_sec && b.tv_nsec >= a.tv_nsec));
  ok &= clock;
  printf("clock_gettime: %s\n", clock ? "ok" : "FAILED");
  // What this runtime does not have fails as on Linux, with errno.
  int absent = fopen("/etc/hostname", "r") == NULL && errno == ENOSYS;
  ok &= absent;
  printf("no file system: fopen fails with ENOSYS: %s\n", absent ? "ok" : "FAILED");
  printf("64-bit: sizeof(long) %zu, sizeof(void *) %zu, pid %d\n", sizeof(long), sizeof(void *), (int)getpid());
  return ok ? 0 : 1;
}
