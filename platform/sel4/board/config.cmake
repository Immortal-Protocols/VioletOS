# Ellis platform for the handset and its QEMU test configuration.
declare_platform(ellis KernelPlatformEllis PLAT_ELLIS KernelArchARM)
if(KernelPlatformEllis)
    declare_seL4_arch(aarch32)
    set(KernelArmCortexA53 ON)
    set(KernelArchArmV8a ON)
    set(KernelArmGicV3 ON)
    config_set(KernelARMPlatform ARM_PLAT ellis)
    list(APPEND KernelDTSList "src/plat/ellis/factory-base.dts" "src/plat/ellis/kernel-overlay.dts")
    # Exercise the handset memory reservations in both supervisors.
    list(APPEND KernelDTSList "src/plat/ellis/moto-radio-memory.dts")
    # Architectural GIC interrupt ID limit; peripheral routing still needs validation.
    set(violet_timer_hz 13000000)
    if(VioletQemuBoard)
        config_set(KernelEllisWifiFixture ELLIS_WIFI_FIXTURE ON)
        set(violet_timer_hz 62500000)
    else()
        add_sources(DEP "KernelPlatformEllis" ASMFILES src/plat/ellis/wifi/monitor.S)
    endif()
    # MCS converts timer ticks to microseconds as (ticks * CLK_MAGIC) >> CLK_SHIFT.
    if(VioletQemuBoard)
        set(violet_clk_magic 4611686019llu)  # 1 / 62.5 MHz, as qemu-arm-virt
        set(violet_clk_shift 58u)
    else()
        set(violet_clk_magic 330382100llu)   # 2^32 / 13 MHz
        set(violet_clk_shift 32u)
    endif()
    declare_default_headers(TIMER_FREQUENCY ${violet_timer_hz} MAX_IRQ 1019 NUM_PPI 32
        TIMER drivers/timer/arm_generic.h INTERRUPT_CONTROLLER arch/machine/gic_v3.h
        CLK_MAGIC ${violet_clk_magic} CLK_SHIFT ${violet_clk_shift} KERNEL_WCET 10u)
endif()
add_sources(DEP "KernelPlatformEllis" CFILES src/arch/arm/machine/gic_v3.c src/arch/arm/machine/l2c_nop.c)
