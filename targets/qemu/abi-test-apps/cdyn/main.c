/* A dynamically linked program: musl's loader (/lib/ld-musl-armhf.so.1, the
 * PT_INTERP the Open Runtime accepts) relocates it, its thread-local storage
 * and its calls into libc.so (make test-abi). */
#include <dlfcn.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>

__thread int tls = 41;

int main(void) {
    char *p = malloc(1 << 20);
    memset(p, 1, 1 << 20);
    tls++;
    printf("cdyn: AT_BASE %s, tls %d, sqrt %.3f, malloc %d\n", getauxval(AT_BASE) ? "set" : "0", tls, sqrt(2.0), p[12345]);
    void *h = dlopen("libc.so", RTLD_NOW);
    printf("cdyn: dlopen libc %s, dlsym %s\n", h ? "ok" : "fail", h && dlsym(h, "printf") ? "ok" : "fail");
    printf("cdyn done\n");
    return 0;
}
