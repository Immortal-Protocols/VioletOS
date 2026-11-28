// The Ellis kernel extension's secure-monitor stubs for the AArch64 handset
// kernel: platform/sel4/board/wifi/monitor.S in A64. The same four MediaTek
// SiP calls with the same fixed arguments, as a 64-bit kernel makes them: the
// SMC64 function IDs (bit 30 set, MediaTek's MTK_SIP_SMC_AARCH_BIT; measured
// on the XT2163DL: the SMC32 IDs from an AArch64 caller return all ones), the
// arguments zero-extended in x1-x4; invoke.h and table.h decide which may be
// made. Only the handset kernel
// assembles this (src/plat/ellis/config.cmake); QEMU's has C stubs that fail.
// The monitor preserves x19-x30 and sp (SMC Calling Convention); x0-x18 are
// the caller's to lose, as for any call.
.text

// MediaTek SiP EMIMPU_CONTROL call.
.global moto_emi_call
moto_emi_call:
    mov     w4, w3
    mov     w3, w2
    mov     w2, w1
    mov     w1, w0
    ldr     w0, =0xc200050b
    mov     w5, #0
    mov     w6, #0
    mov     w7, #0
    smc     #0
    ret

// mt-plat/mtk_secure_api.h: emi_mpu_smc_read(offset).
.global moto_emi_read_offset
moto_emi_read_offset:
    mov     w1, w0
    ldr     w0, =0xc2000261
    mov     w2, #0
    mov     w3, #0
    mov     w4, #0
    mov     w5, #0
    mov     w6, #0
    mov     w7, #0
    smc     #0
    ret

// Factory EMI MPU set-region interface, paired with the offset reader.
.global moto_emi_set_window
moto_emi_set_window:
    mov     w3, w2
    mov     w2, w1
    mov     w1, w0
    ldr     w0, =0xc2000262
    mov     w4, #0
    mov     w5, #0
    mov     w6, #0
    mov     w7, #0
    smc     #0
    ret

// MT6765 secure RNG returns four words. Only the fixed RNG command is issued.
.global moto_random_words
moto_random_words:
    stp     x19, x30, [sp, #-16]!
    mov     x19, x0
    ldr     w0, =0xc200026a
    ldr     w1, =0x74726e67
    mov     w2, #0
    mov     w3, #0
    mov     w4, #0
    mov     w5, #0
    mov     w6, #0
    mov     w7, #0
    smc     #0
    stp     w0, w1, [x19]
    stp     w2, w3, [x19, #8]
    ldp     x19, x30, [sp], #16
    ret
.ltorg
