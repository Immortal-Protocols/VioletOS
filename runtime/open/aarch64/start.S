.text
.global _start
_start:
    adrp x1, root_stack_end
    add x1, x1, :lo12:root_stack_end
    mov sp, x1
    bl root_main
    b .
.bss
.balign 16
.skip 32768
root_stack_end:

// Copied into a user RX page. All interrupted SIMD state stays in the Linux
// AArch64 signal frame, where a SA_SIGINFO handler can inspect or change it.
.section .rodata
.balign 16
.global signal_gate_start, signal_gate_end
signal_gate_start:
    .arch armv8-a+simd
    add x10, sp, #608
    stp q0, q1, [x10, #0]
    stp q2, q3, [x10, #32]
    stp q4, q5, [x10, #64]
    stp q6, q7, [x10, #96]
    stp q8, q9, [x10, #128]
    stp q10, q11, [x10, #160]
    stp q12, q13, [x10, #192]
    stp q14, q15, [x10, #224]
    stp q16, q17, [x10, #256]
    stp q18, q19, [x10, #288]
    stp q20, q21, [x10, #320]
    stp q22, q23, [x10, #352]
    stp q24, q25, [x10, #384]
    stp q26, q27, [x10, #416]
    stp q28, q29, [x10, #448]
    stp q30, q31, [x10, #480]
    mrs x11, fpsr
    mrs x12, fpcr
    stp w11, w12, [x10, #-8]
    blr x3
    add x10, sp, #608
    ldp w11, w12, [x10, #-8]
    msr fpsr, x11
    msr fpcr, x12
    ldp q0, q1, [x10, #0]
    ldp q2, q3, [x10, #32]
    ldp q4, q5, [x10, #64]
    ldp q6, q7, [x10, #96]
    ldp q8, q9, [x10, #128]
    ldp q10, q11, [x10, #160]
    ldp q12, q13, [x10, #192]
    ldp q14, q15, [x10, #224]
    ldp q16, q17, [x10, #256]
    ldp q18, q19, [x10, #288]
    ldp q20, q21, [x10, #320]
    ldp q22, q23, [x10, #352]
    ldp q24, q25, [x10, #384]
    ldp q26, q27, [x10, #416]
    ldp q28, q29, [x10, #448]
    ldp q30, q31, [x10, #480]
    mov x8, #139
    svc #0
    brk #0
signal_gate_end:
