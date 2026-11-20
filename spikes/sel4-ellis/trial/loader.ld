ENTRY(_start)
SECTIONS {
 . = 0x40008000;
 .image : { *(.head) *(.text*) *(.rodata*) *(.data*) *(.payload) . = ALIGN(8); . += 8192; stack_end = .; }
 image_size = SIZEOF(.image);
 kernel_span = kernel_end - kernel;
 /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment*) *(.note*) }
}
ASSERT(image_size < 0x180000, "loader extends into user image")
ASSERT(kernel_span <= 0x100000, "kernel exceeds staging window")
