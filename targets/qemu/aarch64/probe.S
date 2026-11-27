// Freestanding Linux AArch64 ELF. No seL4 libraries, addresses or calls.
.global _start
.text
_start:
    // Match LibGC and the wide virtual arena observed during LibJS startup.
    mov x0, #0
    mov x1, #0x1000000000
    mov x2, #0
    mov x3, #0x4022
    mov x4, #-1
    mov x5, #0
    mov x8, #222
    svc #0
    tbnz x0, #63, failed
    // mmap's unused x7 is deliberately set to a seL4 syscall number.
    mov x7, #-1
    mov x0, #0
    ldr x1, =4432406249472
    mov x2, #0
    mov x3, #0x4022
    mov x4, #-1
    mov x5, #0
    mov x8, #222
    svc #0
    tbnz x0, #63, failed
    mov x19, x0
    ldr x9, =4432406249472
    add x0, x0, x9
    sub x0, x0, #1, lsl #12
    mov x20, x0
    mov x1, #4096
    mov x2, #3
    mov x8, #226
    svc #0
    cbnz x0, failed
    ldr x9, [x20]
    cbnz x9, failed
    mov x9, #42
    str x9, [x20]
    ldr x10, [x20]
    cmp x9, x10
    b.ne failed
    // LibGC releases committed anonymous blocks and later faults them back in.
    mov x0, x20
    mov x1, #4096
    mov x2, #4
    mov x8, #233
    svc #0
    cbnz x0, failed
    ldr x10, [x20]
    cbnz x10, failed
    mov x0, x19
    ldr x1, =4432406249472
    mov x8, #215
    svc #0
    cbnz x0, failed
    mov x0, #1
    adr x1, message
    mov x2, #message_end-message
    mov x8, #64
    svc #0
    mov x0, #0
    b exit
failed:
    mov x0, #1
exit:
    mov x8, #93
    svc #0
message:
    .ascii "VioletOS AArch64: 64 GiB plus 4432406249472-byte reservation and page commit passed\n"
message_end:
