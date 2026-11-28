# Ellis platform, AArch64 kernel (kernel/sel4/aarch64): QEMU virt by default,
# the XT2163DL handset with VioletHandsetBoard=ON. The AArch32 platform file
# (platform/sel4/board/config.cmake) is untouched and stays the default; this
# one replaces it in the 64-bit kernel trees alone.
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
    # The generic timer: QEMU virt's runs at 62.5 MHz, the MT6765's at 13 MHz
    # (as the 32-bit handset kernel). MCS (off here) would convert ticks to
    # microseconds as (ticks * CLK_MAGIC) >> CLK_SHIFT.
    # The EL0 physical timer only for threads with seL4_TCBFlag_ellisTimer
    # (kernel/sel4/aarch64/ellis-timer.patch): the supervisor's timekeeper.
    config_set(KernelEllisTimerGrant ELLIS_TIMER_GRANT ON)
    # The Wi-Fi secure-monitor extension (kernel/sel4/aarch64/ellis-wifi.patch):
    # the four SiP calls on the handset, failing C stubs on QEMU.
    if(VioletHandsetBoard)
        add_sources(DEP "KernelPlatformEllis" ASMFILES src/plat/ellis/wifi/monitor.S)
    else()
        config_set(KernelEllisWifiFixture ELLIS_WIFI_FIXTURE ON)
    endif()
    if(VioletHandsetBoard)
        declare_default_headers(TIMER_FREQUENCY 13000000 MAX_IRQ 1019 NUM_PPI 32
            TIMER drivers/timer/arm_generic.h INTERRUPT_CONTROLLER arch/machine/gic_v3.h
            CLK_MAGIC 330382100llu CLK_SHIFT 32u KERNEL_WCET 10u)
    else()
        declare_default_headers(TIMER_FREQUENCY 62500000 MAX_IRQ 1019 NUM_PPI 32
            TIMER drivers/timer/arm_generic.h INTERRUPT_CONTROLLER arch/machine/gic_v3.h
            CLK_MAGIC 4611686019llu CLK_SHIFT 58u KERNEL_WCET 10u)
    endif()
endif()
add_sources(DEP "KernelPlatformEllis" CFILES src/arch/arm/machine/gic_v3.c src/arch/arm/machine/l2c_nop.c)
