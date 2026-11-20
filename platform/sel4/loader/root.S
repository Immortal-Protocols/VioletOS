.syntax unified
.arm
.section .entry,"ax"
.global _start
_start:
    ldr sp, =0x100000 + ROOT_BYTES
    b root_main
