// Verified boot for the AArch64 system on QEMU (stack.py): what the AArch32
// loader's verified() does (boot/loader/loader.c), for the qemu64 entry stub.
// entry.S calls verify64() with the MMU and caches on, after stage.S has put
// the kernel and the supervisor where they run and before either starts. It
// checks them in place against the boot manifest linked beside this code
// (tools/boot_manifest.py, signed by a key the supervisor's build pins) with
// the same code as the AArch32 loader (boot/loader/verified_lib.rs over
// BearSSL), and the bootloader's rollback floor where tools/run.py puts it
// (tools/boot_manifest.py FLOOR_ADDRESS). The kernel's region and the root
// task's whole window (its image, zeroed stack included) are what is signed.
// Nonzero: start the kernel. Zero: the refusal is printed and nothing starts.
#include <stddef.h>
#include <stdint.h>

struct images {
    const void *manifest; size_t manifest_bytes;
    const void *kernel; size_t kernel_bytes;
    const void *root; size_t root_bytes;
    const void *apps; size_t apps_room;
    unsigned floor[3];
};
extern unsigned verified_boot(const struct images *images, unsigned *version);
extern char boot_manifest[], boot_manifest_end[];

static void say(const char *s) {
    volatile uint32_t *uart = (void *)UART_PA;  // QEMU's PL011
    while (*s) *uart = (unsigned char)*s++;
}
static void say_number(unsigned v) {
    char text[11], *p = text + 10;
    *p = 0;
    do *--p = '0' + v % 10; while (v /= 10);
    say(p);
}

unsigned long verify64(void) {
    const volatile unsigned *floor = (void *)FLOOR_PA;
    const struct images images = {
        boot_manifest, boot_manifest_end - boot_manifest,
        (const void *)KERNEL_PA, KERNEL_SIGNED, (const void *)ROOT_PA, ROOT_SIGNED,
        (const void *)APPS_PA, APPS_ROOM, {floor[0], floor[1], floor[2]}};
    unsigned version = 0, why = verified_boot(&images, &version);
    if (!why) {
        say("VioletOS loader: verified boot, system version ");
        say_number(version);
        say("\n");
        return 1;
    }
    say("VioletOS loader: boot refused, reason ");
    say_number(why);
    say("\n");
    return 0;
}
