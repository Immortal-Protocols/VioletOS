.syntax unified
.arch armv8-a
.arm
.section .head,"ax"
.global _start
_start:
    b start
    .space 32
    .word 0x016f2818, 0, image_size
start:
    cpsid if
    adr r0, _start
    ldr r1, =0x40008000
    cmp r0, r1
    bne halt
    mov r4, r2
    mrs r0, cpsr
    and r0, r0, #31
    cmp r0, #0x1a
    bne svc_entry
    mrc p15, 4, r1, c1, c0, 0
    tst r1, #5
    bne halt
    mov r1, #0
    mcr p15, 4, r1, c1, c1, 0   // stage 2 off, no EL1 traps
    mov r1, #3
    mcr p15, 4, r1, c14, c1, 0  // EL1 timer access
    mrc p15, 4, r1, c12, c9, 5
    orr r1, r1, #9              // allow EL1 GIC system registers
    mcr p15, 4, r1, c12, c9, 5
    isb
    mov r1, #0xd3
    msr spsr_hyp, r1
    adr r1, svc_entry
    msr elr_hyp, r1
    eret
svc_entry:
    mrs r0, cpsr
    and r0, r0, #31
    cmp r0, #0x13
    bne halt
    mrc p15, 0, r1, c1, c0, 0
    tst r1, #5
    bne halt
    ldr sp, =stack_end
    mov r0, r4
    bl prepare
halt:
    wfe
    b halt



// Executed from staging RAM. All literals must travel with this block.
.balign 16
.global trampoline, trampoline_end
trampoline:
    ldr sp, =0x43080000
    ldr r0, =0x43100000
    ldr r1, =0x40000000
    ldr r2, =kernel_span
copy_kernel:
    ldr r3, [r0], #4
    str r3, [r1], #4
    subs r2, r2, #4
    bne copy_kernel
#ifdef VIOLET_MOTO
    // Amber square on the white band: the relocated kernel copy completed.
    ldr r0, =0x79466800         // framebuffer + 32 rows * 720 pixels * 4
    ldr r1, =0xffffae32
    mov r2, #16
1:  mov r3, #24
2:  str r1, [r0], #4
    subs r3, r3, #1
    bne 2b
    add r0, r0, #2784          // next row after 24 pixels
    subs r2, r2, #1
    bne 1b
#endif
    dsb sy
    mov r0, #0
    mcr p15, 0, r0, c7, c5, 0
    mcr p15, 0, r0, c8, c7, 0
    mcr p15, 0, r0, c2, c0, 2
    mcr p15, 0, r0, c13, c0, 1
    ldr r1, =0x43004000
    mcr p15, 0, r1, c2, c0, 0
    mov r1, #1
    mcr p15, 0, r1, c3, c0, 0
    dsb sy
    isb
    mrc p15, 0, r1, c1, c0, 0
    orr r1, r1, #1
    mcr p15, 0, r1, c1, c0, 0
    isb
    push {r0,r1}                // omit DTB, board configuration is compiled in
    str r0, [sp,#4]
    ldr r0, =0x40200000
    ldr r1, =0x40200000 + ROOT_BYTES
    ldr r2, =0x40100000
    ldr r3, =0x00100000
    ldr pc, =0xe0000000
    .ltorg
trampoline_end:
