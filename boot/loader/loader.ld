ENTRY(_start)
SECTIONS {
 . = 0x40008000;
 /* Zero-initialized statics are part of the image: nothing clears .bss before C runs. */
 .image : { *(.head) *(.text*) *(.rodata*) *(.data*) *(.bss*) *(COMMON) . = ALIGN(8); . += 24576; stack_end = .; *(.payload) }
 image_size = SIZEOF(.image);
 kernel_span = kernel_end - kernel;
 /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment*) *(.note*) }
}
/* Radio firmware is staged before the destination of the root image is cleared. */
ASSERT(root_end <= 0x40200000 && stack_end <= 0x40200000, "live loader overlaps root destination")
ASSERT(ADDR(.image) + SIZEOF(.image) <= 0x41000000, "payload overlaps reserved DMA memory")
ASSERT(kernel_span <= 0x100000, "kernel exceeds staging window")
