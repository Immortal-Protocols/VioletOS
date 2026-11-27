PHDRS {
    image PT_LOAD FILEHDR PHDRS FLAGS(5);
    dynamic PT_DYNAMIC FLAGS(4);
}
SECTIONS {
    . = SIZEOF_HEADERS;
    .hash : { *(.hash) } :image
    .dynsym : { *(.dynsym) } :image
    .dynstr : { *(.dynstr) } :image
    .gnu.version : { *(.gnu.version) } :image
    .gnu.version_d : { *(.gnu.version_d) } :image
    .dynamic : { *(.dynamic) } :image :dynamic
    .text : { *(.text*) } :image
    .rodata : { *(.rodata*) } :image
    .violet_clock 0xff0 : { KEEP(*(.violet_clock)) } :image
    ASSERT(SIZEOF(.violet_clock) == 16, "vDSO clock layout")
    /DISCARD/ : { *(.comment .note* .eh_frame*) }
}
VERSION { LINUX_2.6.39 { global: __kernel_clock_gettime; __kernel_gettimeofday; local: *; }; }
