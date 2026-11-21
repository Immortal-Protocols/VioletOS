.syntax unified
.arch armv8-a
.arm
.text
// Standard 32-bit MediaTek SiP EMIMPU_CONTROL call. No app receives this interface.
.global moto_emi_call
moto_emi_call:
    push {r4-r7,lr}
    mov r4,r3
    mov r3,r2
    mov r2,r1
    mov r1,r0
    ldr r0,=0x8200050b
    mov r5,#0
    mov r6,#0
    mov r7,#0
    smc #0
    pop {r4-r7,pc}

// mt-plat/mtk_secure_api.h: emi_mpu_smc_read(offset).
.global moto_emi_read_offset
moto_emi_read_offset:
    push {r4-r7,lr}
    mov r1,r0
    ldr r0,=0x82000261
    mov r2,#0
    mov r3,#0
    mov r4,#0
    mov r5,#0
    mov r6,#0
    mov r7,#0
    smc #0
    pop {r4-r7,pc}

// Factory EMI MPU set-region interface, paired with the offset reader.
.global moto_emi_set_window
moto_emi_set_window:
    push {r4-r7,lr}
    mov r3,r2
    mov r2,r1
    mov r1,r0
    ldr r0,=0x82000262
    mov r4,#0
    mov r5,#0
    mov r6,#0
    mov r7,#0
    smc #0
    pop {r4-r7,pc}

// MT6765 secure RNG returns four words. Only the fixed RNG command is issued.
.global moto_random_words
moto_random_words:
    push {r4-r8,lr}
    mov r8,r0
    ldr r0,=0x8200026a
    ldr r1,=0x74726e67
    mov r2,#0
    mov r3,#0
    mov r4,#0
    mov r5,#0
    mov r6,#0
    mov r7,#0
    smc #0
    stmia r8,{r0-r3}
    pop {r4-r8,pc}
