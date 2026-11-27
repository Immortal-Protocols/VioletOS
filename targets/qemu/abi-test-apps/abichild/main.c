/* Started by the other ABI test programs (no icon): it reports its argv,
 * environment and descriptors, and exits with a status they check. */
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : "";
    if (!strcmp(mode, "exec")) {
        printf("abichild: exec ok, parent %d\n", getppid() > 1);
        return 7;
    }
    if (!strcmp(mode, "spawn") && argc > 2) {
        const char *e = getenv("ABI");
        printf("env %s cloexec %s", e ? e : "none", fcntl(atoi(argv[2]), F_GETFD) == -1 ? "closed" : "open");
        fflush(stdout);
        return 3;
    }
    if (!strcmp(mode, "kill-parent")) return kill(getppid(), 15) == -1 ? 0 : 1;
    printf("child %s\n", mode);
    return !strcmp(mode, "rust") ? 4 : 0;
}
