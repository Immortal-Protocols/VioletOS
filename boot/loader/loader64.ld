/* The AArch64 handset loader (loader64.S, loader.c): an arm64 Image that LK
   decompresses to the boot header's kernel_addr, 0x40080000. */
ENTRY(_start)
SECTIONS {
 . = 0x40080000;
 /* Zero-initialized statics are part of the image: nothing clears .bss before C runs. */
 .image : { *(.head) *(.text*) *(.rodata*) *(.data*) *(.got*) *(.bss*) *(COMMON) . = ALIGN(16); . += 65536; stack_end = .;
            *(.payload) . = ALIGN(4096); }
 image_size = SIZEOF(.image);
 /DISCARD/ : { *(.comment*) *(.note*) *(.eh_frame*) }
}
/* The image must stay below the hole at 0x41000000 and clear of the windows
   prepare() writes: the root task's, the trampoline and boot tables at
   0x43000000, the kernel's staging copy, the catalog and the firmware. */
ASSERT(ADDR(.image) + SIZEOF(.image) <= 0x41000000, "the loader image reaches the reserved memory at 0x41000000")
ASSERT(ADDR(.image) + SIZEOF(.image) <= ROOT_PA, "the loader image overlaps the root task's window")
ASSERT(trampoline64_end - trampoline64 <= 0x1000, "the trampoline outgrew its page")
ASSERT(kernel_end - kernel <= 0x100000, "the kernel exceeds its staging window")
