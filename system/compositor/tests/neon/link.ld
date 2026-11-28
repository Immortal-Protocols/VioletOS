ENTRY(_start)
SECTIONS {
 . = 0x40010000;
 .text : { KEEP(*(.entry)) *(.text*) *(.rodata*) }
 .data : { *(.data*) }
 .bss (NOLOAD) : { *(.bss*) *(COMMON) }
 . = ALIGN(16);
 . += 0x10000;
 stack_top = .;
 /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) }
}
