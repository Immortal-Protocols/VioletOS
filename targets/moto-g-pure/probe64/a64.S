// AArch64 half of the XT2163DL entry probe (see README.md, "AArch64 boot path"):
// mask DAIF, route every exception to a TOPRGU reset, kick the watchdog if it
// is armed, paint a band by EL, set the MT6357 RTC fast-boot flag (RTC_PDN1
// bit 13, as power.rs set_fastboot does) and reset. All loops are bounded.
        .text
start:
        msr     daifset, #0xf
        mov     x19, x0                 // DTB from the bootloader
        movz    x20, #0x1000, lsl #16
        movk    x20, #0x7000            // TOPRGU 0x10007000
        movz    x21, #0x1000, lsl #16
        movk    x21, #0xd000            // PMIC wrapper 0x1000d000
        // Any exception (an undefined smc, a bus fault) ends in the TOPRGU reset.
        adr     x1, vectors
        mrs     x2, CurrentEL
        cmp     x2, #8
        b.ne    4f
        msr     vbar_el2, x1
        b       5f
4:      msr     vbar_el1, x1
5:      isb
        bl      kick
        // Band: rows 600..799 of the 720-px LK framebuffer at 0x79450000,
        // colour by EL (EL2 magenta, EL1 cyan, other yellow).
        mrs     x22, CurrentEL
        lsr     x22, x22, #2
        movz    w1, #0xffff, lsl #16
        movk    w1, #0x00ff             // 0xffff00ff
        cmp     x22, #2
        b.eq    1f
        movz    w1, #0xff00, lsl #16
        movk    w1, #0xffff             // 0xff00ffff
        cmp     x22, #1
        b.eq    1f
        movz    w1, #0xffff, lsl #16
        movk    w1, #0xff00             // 0xffffff00
1:      movz    x2, #0x7945, lsl #16
        movz    x3, #0x1a, lsl #16
        movk    x3, #0x5e00             // 600 rows * 720 * 4
        add     x2, x2, x3
        movz    x3, #0x2, lsl #16
        movk    x3, #0x3280             // 200 rows * 720 px
2:      str     w1, [x2], #4
        subs    x3, x3, #1
        b.ne    2b
        dsb     sy
        bl      kick
        // ready(): wrapper initialised, WACS2 ready, PMIC id 0x57xx.
        ldr     w1, [x21, #0xa0]
        tbz     w1, #0, reset
        ldr     w1, [x21, #0xc24]
        tbz     w1, #22, reset
        mov     w0, #0x0a
        bl      pmic_read
        lsr     w1, w0, #8
        cmp     w1, #0x57
        b.ne    reset
        // Unlock the RTC (RTC_PROT keys), set RTC_PDN1 bit 13, commit.
        mov     w0, #0x5bc
        mov     w1, #0x586a
        bl      pmic_write
        bl      rtc_commit
        mov     w0, #0x5bc
        mov     w1, #0x9136
        bl      pmic_write
        bl      rtc_commit
        mov     w0, #0x5b4
        bl      pmic_read
        orr     w1, w0, #0x2000
        mov     w0, #0x5b4
        bl      pmic_write
        bl      rtc_commit
reset:
        mov     w1, #0x1209
        str     w1, [x20, #0x14]        // TOPRGU SWRST
        dsb     sy
        movz    x3, #0x100, lsl #16
3:      subs    x3, x3, #1
        b.ne    3b
        movz    w0, #0x8400, lsl #16
        movk    w0, #0x0009             // PSCI SYSTEM_RESET
        smc     #0
        b       reset

kick:   // If the watchdog is armed, restart its count.
        ldr     w1, [x20]
        tbz     w1, #0, 1f
        mov     w1, #0x1971
        str     w1, [x20, #8]
        dsb     sy
1:      ret

// wait_state(w9 = wanted): w0 = status, or a jump to reset after the bound.
wait_state:
        movz    x10, #0x40, lsl #16     // about 4M reads
1:      ldr     w0, [x21, #0xc24]
        ubfx    w11, w0, #16, #3
        cmp     w11, w9
        b.eq    2f
        cbnz    w9, 3f
        cmp     w11, #6                 // an uncollected read: clear it
        b.ne    3f
        mov     w12, #1
        str     w12, [x21, #0xc28]
        dsb     sy
3:      subs    x10, x10, #1
        b.ne    1b
        b       reset
2:      ret

// pmic_read(w0 = addr) -> w0 = value
pmic_read:
        mov     x28, x30
        mov     w13, w0
        mov     w9, #0
        bl      wait_state
        lsl     w12, w13, #15
        str     w12, [x21, #0xc20]
        dsb     sy
        mov     w9, #6
        bl      wait_state
        and     w13, w0, #0xffff
        mov     w12, #1
        str     w12, [x21, #0xc28]
        dsb     sy
        mov     w0, w13
        ret     x28

// pmic_write(w0 = addr, w1 = value)
pmic_write:
        mov     x28, x30
        mov     w13, w0
        mov     w14, w1
        mov     w9, #0
        bl      wait_state
        lsl     w12, w13, #15
        orr     w12, w12, w14
        orr     w12, w12, #0x80000000
        str     w12, [x21, #0xc20]
        dsb     sy
        mov     w9, #0
        bl      wait_state
        ret     x28

// rtc_commit: RTC_WRTGR = 1, then wait (bounded) for RTC_BBPU busy (bit 6) to clear.
rtc_commit:
        mov     x27, x30
        mov     w0, #0x5c2
        mov     w1, #1
        bl      pmic_write
        mov     x26, #500
1:      bl      kick
        mov     w0, #0x588
        bl      pmic_read
        tbz     w0, #6, 2f
        movz    x3, #0x20, lsl #16      // about a millisecond
3:      subs    x3, x3, #1
        b.ne    3b
        subs    x26, x26, #1
        b.ne    1b
        b       reset
2:      ret     x27
fault:  mov     w1, #0x1209
        str     w1, [x20, #0x14]
        dsb     sy
        b       fault
        .balign 2048
vectors:
        .rept   16
        b       fault
        .balign 128
        .endr
        .balign 4096
end:
