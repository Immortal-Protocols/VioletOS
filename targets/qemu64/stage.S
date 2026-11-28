// QEMU writes its own device tree at the start of RAM when the machine
// resets, so the image cannot be loaded there directly (the AArch32 entry
// stub has the same problem). QEMU loads this stub and the image at a staging
// address; the stub copies the image to the start of RAM and jumps to the
// entry stub inside it. Position independent: it runs where QEMU put it.
.section .text
.global _start
_start:
    adr     x0, image
    ldr     x1, =IMAGE_BASE
    ldr     x2, =IMAGE_BYTES
copy:
    ldr     x3, [x0], #8
    str     x3, [x1], #8
    subs    x2, x2, #8
    b.gt    copy
    dsb     sy
    isb
    ldr     x9, =ENTRY
    br      x9
.ltorg
.balign 4096
image:
