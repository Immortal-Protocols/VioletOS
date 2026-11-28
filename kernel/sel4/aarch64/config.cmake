# Ellis platform, AArch64 kernel, QEMU test configuration only (kernel/sel4/aarch64).
# The AArch32 platform file (platform/sel4/board/config.cmake) is untouched and
# stays the default; this one replaces it in the 64-bit kernel tree alone.
declare_platform(ellis KernelPlatformEllis PLAT_ELLIS KernelArchARM)
if(KernelPlatformEllis)
    declare_seL4_arch(aarch64)
    set(KernelArmCortexA53 ON)
    set(KernelArchArmV8a ON)
    set(KernelArmGicV3 ON)
    config_set(KernelARMPlatform ARM_PLAT ellis)
    list(APPEND KernelDTSList "src/plat/ellis/factory-base.dts" "src/plat/ellis/kernel-overlay.dts")
    # The same memory reservations as the 32-bit kernels, so one memory map serves both.
    list(APPEND KernelDTSList "src/plat/ellis/moto-radio-memory.dts")
    # QEMU virt's generic timer: 62.5 MHz. MCS converts ticks to microseconds
    # as (ticks * CLK_MAGIC) >> CLK_SHIFT.
    declare_default_headers(TIMER_FREQUENCY 62500000 MAX_IRQ 1019 NUM_PPI 32
        TIMER drivers/timer/arm_generic.h INTERRUPT_CONTROLLER arch/machine/gic_v3.h
        CLK_MAGIC 4611686019llu CLK_SHIFT 58u KERNEL_WCET 10u)
endif()
add_sources(DEP "KernelPlatformEllis" CFILES src/arch/arm/machine/gic_v3.c src/arch/arm/machine/l2c_nop.c)
