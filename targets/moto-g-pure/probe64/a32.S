// AArch32 half of the polyglot probe, reached by word 0 (b 0x84) when the
// bootloader enters the image in AArch32. It sets the RTC fast-boot flag and
// resets at once, so fastboot comes back within a few seconds.
        .syntax unified
        .arm
        .text
a32:    cpsid   aif
        movw    r4, #0x7000
        movt    r4, #0x1000             @ TOPRGU
        movw    r5, #0xd000
        movt    r5, #0x1000             @ PMIC wrapper
        bl      kick
        ldr     r1, [r5, #0xa0]
        tst     r1, #1
        beq     reset
        ldr     r1, [r5, #0xc24]
        tst     r1, #(1 << 22)
        beq     reset
        mov     r0, #0x0a
        bl      pmic_read
        lsr     r1, r0, #8
        cmp     r1, #0x57
        bne     reset
        movw    r0, #0x5bc
        movw    r1, #0x586a
        bl      pmic_write
        bl      rtc_commit
        movw    r0, #0x5bc
        movw    r1, #0x9136
        bl      pmic_write
        bl      rtc_commit
        movw    r0, #0x5b4
        bl      pmic_read
        orr     r1, r0, #0x2000
        movw    r0, #0x5b4
        bl      pmic_write
        bl      rtc_commit
reset:  movw    r1, #0x1209
        str     r1, [r4, #0x14]
        dsb     sy
        b       reset

kick:   ldr     r1, [r4]
        tst     r1, #1
        bxeq    lr
        movw    r1, #0x1971
        str     r1, [r4, #8]
        dsb     sy
        bx      lr

@ wait_state(r9 = wanted) -> r0 = status
wait_state:
        mov     r10, #0x400000
1:      ldr     r0, [r5, #0xc24]
        ubfx    r6, r0, #16, #3
        cmp     r6, r9
        bxeq    lr
        cmp     r9, #0
        bne     3f
        cmp     r6, #6
        bne     3f
        mov     r7, #1
        str     r7, [r5, #0xc28]
        dsb     sy
3:      subs    r10, r10, #1
        bne     1b
        b       reset

pmic_read:
        mov     r11, lr
        mov     r8, r0
        mov     r9, #0
        bl      wait_state
        lsl     r7, r8, #15
        str     r7, [r5, #0xc20]
        dsb     sy
        mov     r9, #6
        bl      wait_state
        uxth    r8, r0
        mov     r7, #1
        str     r7, [r5, #0xc28]
        dsb     sy
        mov     r0, r8
        bx      r11

pmic_write:
        mov     r11, lr
        mov     r8, r0
        mov     r3, r1
        mov     r9, #0
        bl      wait_state
        lsl     r7, r8, #15
        orr     r7, r7, r3
        orr     r7, r7, #0x80000000
        str     r7, [r5, #0xc20]
        dsb     sy
        mov     r9, #0
        bl      wait_state
        bx      r11

rtc_commit:
        mov     r12, lr
        movw    r0, #0x5c2
        mov     r1, #1
        bl      pmic_write
        movw    r2, #500
1:      bl      kick
        movw    r0, #0x588
        bl      pmic_read
        tst     r0, #(1 << 6)
        bxeq    r12
        mov     r3, #0x200000
3:      subs    r3, r3, #1
        bne     3b
        subs    r2, r2, #1
        bne     1b
        b       reset
