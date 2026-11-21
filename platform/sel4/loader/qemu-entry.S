// QEMU places its own ROM DTB at the Moto load address. Copy our zImage there
// after reset, then reproduce the AArch32 kernel entry register convention.
.syntax unified
.arm
.section .text
.global _start
_start:
    // The Moto boot firmware configures the interrupt groups. Reproduce the
    // nonsecure Group 1 routing for the virtual timer PPI on QEMU virt.
    ldr r0, =0x080b0080
    mvn r1, #0
    str r1, [r0]
    dsb sy
    ldr r0, =0x45000000
    ldr r1, =0x40008000
    ldr r2, =IMAGE_BYTES
copy:
    ldr r3, [r0], #4
    str r3, [r1], #4
    subs r2, r2, #4
    bne copy
    dsb sy
    isb
    ldr r2, =0x47880000
    ldr pc, =0x40008000
