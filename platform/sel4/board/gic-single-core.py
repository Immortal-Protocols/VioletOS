"""Add an Ellis-only GIC mapping to the disposable upstream source checkout."""
from pathlib import Path
import sys
src=Path(sys.argv[1])
rules=src/"tools/hardware.yml"
text=rules.read_text()
anchor="  - compatible:\n      - arm,gic-v3"
custom=anchor.replace("arm,gic-v3","violet,ellis-boot-gic")+"\n    regions:\n      - {index: 0, kernel: GICD_PPTR, kernel_size: 0x10000}\n      - {index: 1, kernel: GICR_PPTR, kernel_size: 0x20000}\n    interrupts: {INTERRUPT_VGIC_MAINTENANCE: 0}\n"
if "violet,ellis-boot-gic" not in text:
    assert text.count(anchor)==1
    rules.write_text(text.replace(anchor,custom+anchor))
driver=src/"src/arch/arm/machine/gic_v3.c"
text=driver.read_text()
anchor="#define GICR_SIZE           (0x100000)"
if "CONFIG_PLAT_ELLIS" not in text:
    assert text.count(anchor)==1
    driver.write_text(text.replace(anchor,"#ifdef CONFIG_PLAT_ELLIS\n#define GICR_SIZE GICR_PER_CORE_SIZE\n#else\n"+anchor+"\n#endif"))
