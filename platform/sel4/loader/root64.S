// The supervisor's entry on an AArch64 kernel (root.S is the AArch32 one).
// The kernel starts the root thread with the BootInfo pointer in x0, which
// root_main takes as its argument. The stack is the top of the root task's
// window, as on AArch32; it is 16-byte aligned, as AArch64 requires.
.section .entry,"ax"
.global _start
_start:
    ldr x9, =0x100000 + ROOT_BYTES
    mov sp, x9
    b root_main
