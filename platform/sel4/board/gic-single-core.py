"""Add an Ellis-only GIC mapping to the disposable upstream source checkout:
the distributor and one 128 KiB redistributor frame per core the kernel
boots (KernelMaxNumNodes, at most 4: cluster 0 of the MT6765)."""
from pathlib import Path
import re
import sys
src=Path(sys.argv[1])
FRAMES=0x80000  # four cores' redistributors; the kernel uses the frames it has cores for
rules=src/"tools/hardware.yml"
text=rules.read_text()
anchor="  - compatible:\n      - arm,gic-v3"
custom=anchor.replace("arm,gic-v3","violet,ellis-boot-gic")+f"\n    regions:\n      - {{index: 0, kernel: GICD_PPTR, kernel_size: 0x10000}}\n      - {{index: 1, kernel: GICR_PPTR, kernel_size: {FRAMES:#x}}}\n    interrupts: {{INTERRUPT_VGIC_MAINTENANCE: 0}}\n"
if "violet,ellis-boot-gic" not in text:
    assert text.count(anchor)==1
    rules.write_text(text.replace(anchor,custom+anchor))
else:
    rules.write_text(re.sub(r"(index: 1, kernel: GICR_PPTR, kernel_size: )0x[0-9a-f]+", rf"\g<1>{FRAMES:#x}", text))
driver=src/"src/arch/arm/machine/gic_v3.c"
text=driver.read_text()
anchor="#define GICR_SIZE           (0x100000)"
mine="#ifdef CONFIG_PLAT_ELLIS\n#define GICR_SIZE (GICR_PER_CORE_SIZE * CONFIG_MAX_NUM_NODES)\n#else\n"+anchor+"\n#endif"
if "CONFIG_PLAT_ELLIS" not in text:
    assert text.count(anchor)==1
    driver.write_text(text.replace(anchor,mine))
else:
    driver.write_text(text.replace("#define GICR_SIZE GICR_PER_CORE_SIZE\n","#define GICR_SIZE (GICR_PER_CORE_SIZE * CONFIG_MAX_NUM_NODES)\n"))
