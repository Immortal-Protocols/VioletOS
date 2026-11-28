/* No handler: a fault ends the program as it always has (status 139, the
 * runtime's "bad memory access" line), make test-system32 checks. */
#include <stdio.h>

int main(void) {
    printf("cfault: about to fault\n");
    fflush(stdout);
    *(volatile int *)32 = 1;
    printf("cfault: still running\n");
    return 0;
}
