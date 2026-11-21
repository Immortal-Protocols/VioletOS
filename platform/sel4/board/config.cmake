# Ellis platform for the handset and its QEMU test configuration.
declare_platform(ellis KernelPlatformEllis PLAT_ELLIS KernelArchARM)
if(KernelPlatformEllis)
    declare_seL4_arch(aarch32)
    set(KernelArmCortexA53 ON)
    set(KernelArchArmV8a ON)
    set(KernelArmGicV3 ON)
    config_set(KernelARMPlatform ARM_PLAT ellis)
    list(APPEND KernelDTSList "src/plat/ellis/factory-base.dts" "src/plat/ellis/kernel-overlay.dts")
    # Architectural GIC interrupt ID limit; peripheral routing still needs validation.
    set(violet_timer_hz 13000000)
    if(VioletQemuBoard)
        set(violet_timer_hz 62500000)
    endif()
    declare_default_headers(TIMER_FREQUENCY ${violet_timer_hz} MAX_IRQ 1019 NUM_PPI 32
        TIMER drivers/timer/arm_generic.h INTERRUPT_CONTROLLER arch/machine/gic_v3.h)
endif()
add_sources(DEP "KernelPlatformEllis" CFILES src/arch/arm/machine/gic_v3.c src/arch/arm/machine/l2c_nop.c)
