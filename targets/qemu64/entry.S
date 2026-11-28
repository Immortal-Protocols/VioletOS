// The qemu64 entry stub. QEMU starts the CPU here at EL1 with the MMU off;
// the AArch64 kernel expects to be entered at its virtual address with the
// MMU on and its image mapped (upstream boots through seL4's elfloader, which
// VioletOS does not use). The boot page tables follow this code in the image
// (image.py builds them): an identity map of the first 4 GiB for this stub,
// and the kernel's window. The kernel replaces both with its own tables.
//
// On entry to the kernel: x0, x1 the bootstrap task image's physical range,
// x2 its physical minus virtual offset, x3 its entry point, x4 and x5 the
// device tree (none is passed).
.section .text
.global _start
_start:
    msr     daifset, #0xf
    mrs     x9, CurrentEL
    cmp     x9, #(1 << 2)
    b.ne    stop                    // only EL1: the kernel is not built as a hypervisor
    // As the handset's firmware does: every private interrupt of this core
    // is non-secure Group 1, so the kernel's timer interrupt is delivered.
    ldr     x9, =GICR_IGROUPR0
    mov     w10, #0xffffffff
    str     w10, [x9]
    // Memory attributes by index, as the kernel's page tables use them:
    // 0 device nGnRnE, 1 device nGnRE, 2 device GRE, 3 normal uncached,
    // 4 normal, 5 normal write-through.
    ldr     x9, =0x0000aaff440c0400
    msr     mair_el1, x9
    // 48-bit addresses in both halves, 4 KiB granule, cached inner-shareable
    // table walks, 16-bit ASIDs; the physical address size the CPU reports.
    ldr     x9, =((16) | (1 << 8) | (1 << 10) | (3 << 12) | (16 << 16) | (1 << 24) | (1 << 26) | (3 << 28) | (2 << 30) | (1 << 36))
    mrs     x10, id_aa64mmfr0_el1
    bfi     x9, x10, #32, #3
    msr     tcr_el1, x9
    ldr     x9, =TABLE_LOWER
    msr     ttbr0_el1, x9
    ldr     x9, =TABLE_UPPER
    msr     ttbr1_el1, x9
    isb
    tlbi    vmalle1
    dsb     sy
    isb
    mrs     x9, sctlr_el1
    orr     x9, x9, #(1 << 0)       // MMU
    orr     x9, x9, #(1 << 2)       // data cache
    orr     x9, x9, #(1 << 12)      // instruction cache
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
stop:
    wfi
    b       stop
.ltorg
