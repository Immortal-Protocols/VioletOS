// The XT2163DL loader's AArch64 entry (loader.S is the AArch32 one; the C,
// loader.c, is shared). MediaTek LK starts the kernel of a boot image whose
// command line says bootopt=...,64N2 in AArch64, at EL2, with the MMU off,
// x0 the device tree, and the Image (decompressed from Image.gz) at the
// boot header's kernel_addr, 0x40080000 (targets/moto-g-pure/README.md,
// "AArch64 boot path"). This file:
//
// - carries the arm64 Image header LK's 64-bit path expects;
// - moves the image to its link address if LK put it anywhere else;
// - drops from EL2 to EL1 (the kernel is built for EL1: kernel/sel4/aarch64),
//   giving EL1 the timer, the GIC system registers and FP/SIMD, as loader.S
//   does from HYP to SVC;
// - routes every exception to a TOPRGU reset, so a fault ends the try in
//   seconds instead of the watchdog's half minute;
// - runs loader.c's prepare(x0) at EL1, MMU and data cache off, I-cache on;
// - and holds the trampoline prepare() copies to 0x43000000 and jumps to: it
//   moves the kernel into place, invalidates the caches, turns the MMU on
//   with the boot page tables (targets/qemu64/image.py boot_tables, the
//   same as qemu64's entry.S) and enters seL4 at its virtual entry with the
//   arguments seL4's elfloader would pass.
.section .head,"ax"
.global _start
_start:
    b       start                   // code0
    .long   0                       // code1
    .quad   0                       // text_offset
    .quad   image_size              // image_size
    .quad   0xa                     // flags: little endian, 4 KiB pages, placement independent
    .quad   0, 0, 0
    .ascii  "ARM\x64"               // magic, at 0x38
    .long   0
start:
    msr     daifset, #0xf
    mov     x19, x0                 // the device tree
    mrs     x20, CurrentEL
    adr     x1, _start
    ldr     x2, =_start
    cmp     x1, x2
    b.eq    placed
    // Somewhere else: move the image to its link address (either direction)
    // and continue there. Position independent until the jump.
    ldr     x3, =image_size
    cmp     x2, x1
    b.hi    backward
1:  ldr     x4, [x1], #8
    str     x4, [x2], #8
    subs    x3, x3, #8
    b.gt    1b
    b       moved
backward:
    add     x1, x1, x3
    add     x2, x2, x3
1:  ldr     x4, [x1, #-8]!
    str     x4, [x2, #-8]!
    subs    x3, x3, #8
    b.gt    1b
moved:
    dsb     sy
    ic      iallu
    dsb     sy
    isb
    ldr     x9, =placed
    br      x9
placed:
    lsr     x9, x20, #2
    cmp     x9, #1
    b.eq    el1
    cmp     x9, #2
    b.ne    halt
    adr     x1, vectors
    msr     vbar_el2, x1
    mov     x1, #(1 << 31)          // HCR_EL2: EL1 is AArch64; no traps, no stage 2
    msr     hcr_el2, x1
    msr     vttbr_el2, xzr
    mov     x1, #3                  // CNTHCTL_EL2: EL1 physical counter and timer
    msr     cnthctl_el2, x1
    msr     cntvoff_el2, xzr        // the virtual counter (the kernel's timer) is the physical one
    mov     x1, #0x33ff             // CPTR_EL2: no FP/SIMD trap
    msr     cptr_el2, x1
    mrs     x1, pmcr_el0            // MDCR_EL2: every counter to EL1, no debug traps
    ubfx    x1, x1, #11, #5
    msr     mdcr_el2, x1
    mrs     x1, midr_el1            // EL1 reads the real identity
    msr     vpidr_el2, x1
    mrs     x1, mpidr_el1
    msr     vmpidr_el2, x1
    mrs     x1, id_aa64pfr0_el1     // GICv3 system registers for EL1 (loader.S: ICC_HSRE |= 9)
    ubfx    x1, x1, #24, #4
    cbz     x1, 1f
    mrs     x1, S3_4_C12_C9_5       // ICC_SRE_EL2
    orr     x1, x1, #1              // SRE
    orr     x1, x1, #8              // Enable (lower levels may use the system registers)
    msr     S3_4_C12_C9_5, x1
    isb
    msr     S3_4_C12_C11_0, xzr     // ICH_HCR_EL2: no virtual interface
1:  ldr     x1, =0x30d00800         // SCTLR_EL1: MMU, caches off
    msr     sctlr_el1, x1
    mov     x1, #0x3c5              // EL1h, DAIF masked
    msr     spsr_el2, x1
    adr     x1, el1
    msr     elr_el2, x1
    isb
    eret
el1:
    adr     x1, vectors
    msr     vbar_el1, x1
    ic      iallu
    dsb     sy
    isb
    ldr     x1, =0x30d01800         // SCTLR_EL1: MMU and data cache off, instruction cache on
    msr     sctlr_el1, x1
    mov     x1, #(3 << 20)          // CPACR_EL1: FP/SIMD (the loader's Rust uses it)
    msr     cpacr_el1, x1
    isb
    ldr     x1, =stack_end
    mov     sp, x1
    adr     x1, entry_el
    str     w20, [x1]
    mov     x0, x19
    bl      prepare
halt:
    wfe
    b       halt

// Any exception: a TOPRGU software reset (the try ends now).
fault:
    ldr     x1, =0x10007000
    mov     w2, #0x1209
    str     w2, [x1, #0x14]
    dsb     sy
    b       fault
.ltorg
.balign 4
.global entry_el
entry_el: .long 0

.balign 2048
vectors:
    .rept   16
    b       fault
    .balign 128
    .endr

// Executed from 0x43000000, MMU off: everything it uses travels with it.
.balign 16
.global trampoline64, trampoline64_end
trampoline64:
    ldr     x0, =KERNEL_STAGE
    ldr     x1, =KERNEL_PA
    ldr     x2, =KERNEL_FILE
1:  ldr     x3, [x0], #8
    str     x3, [x1], #8
    subs    x2, x2, #8
    b.gt    1b
    ldr     x2, =(KERNEL_SPAN - KERNEL_FILE)
2:  cbz     x2, 3f
    str     xzr, [x1], #8
    sub     x2, x2, #8
    b       2b
3:  dsb     sy
    // Invalidate (not clean) the data and unified caches by set and way:
    // everything since LK handed over was written with the cache off, so
    // a line still held from before must not come back over it.
    mrs     x0, clidr_el1
    and     x3, x0, #0x07000000
    lsr     x3, x3, #23             // level of coherence * 2
    cbz     x3, 9f
    mov     x10, #0                 // level * 2
4:  add     x2, x10, x10, lsr #1
    lsr     x1, x0, x2
    and     x1, x1, #7              // this level's cache type
    cmp     x1, #2
    b.lt    8f                      // no data cache here
    msr     csselr_el1, x10
    isb
    mrs     x1, ccsidr_el1
    and     x2, x1, #7
    add     x2, x2, #4              // log2(line bytes)
    mov     x4, #0x3ff
    and     x4, x4, x1, lsr #3      // ways - 1
    clz     w5, w4                  // way shift
    mov     x7, #0x7fff
    and     x7, x7, x1, lsr #13     // sets - 1
5:  mov     x9, x4
6:  lsl     x6, x9, x5
    orr     x11, x10, x6
    lsl     x6, x7, x2
    orr     x11, x11, x6
    dc      isw, x11
    subs    x9, x9, #1
    b.ge    6b
    subs    x7, x7, #1
    b.ge    5b
8:  add     x10, x10, #2
    cmp     x3, x10
    b.gt    4b
9:  msr     csselr_el1, xzr
    dsb     sy
    ic      iallu
    tlbi    vmalle1
    dsb     sy
    isb
    // As targets/qemu64/entry.S: memory attributes, 48-bit halves with
    // 4 KiB granules, the boot page tables, then MMU and caches on.
    ldr     x9, =0x0000aaff440c0400
    msr     mair_el1, x9
    ldr     x9, =((16) | (1 << 8) | (1 << 10) | (3 << 12) | (16 << 16) | (1 << 24) | (1 << 26) | (3 << 28) | (2 << 30) | (1 << 36))
    mrs     x10, id_aa64mmfr0_el1
    bfi     x9, x10, #32, #3
    msr     tcr_el1, x9
    ldr     x9, =TABLE_LOWER
    msr     ttbr0_el1, x9
    ldr     x9, =TABLE_UPPER
    msr     ttbr1_el1, x9
    isb
    mrs     x9, sctlr_el1
    orr     x9, x9, #(1 << 0)
    orr     x9, x9, #(1 << 2)
    orr     x9, x9, #(1 << 12)
    msr     sctlr_el1, x9
    isb
    ldr     x0, =ROOT_PA_START
    ldr     x1, =ROOT_PA_END
    ldr     x2, =ROOT_PV_OFFSET
    ldr     x3, =ROOT_ENTRY
    mov     x4, #0
    mov     x5, #0
    ldr     x9, =KERNEL_ENTRY
    br      x9
.ltorg
trampoline64_end:
