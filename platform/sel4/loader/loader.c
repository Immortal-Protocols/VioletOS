#include <libfdt.h>
#include <stdint.h>
#include <string.h>

extern char kernel[], kernel_end[], root[], root_end[], trampoline[], trampoline_end[];
enum { ram = 0x40000000, ram_end = 0x44000000, fb = 0x79450000, pool = 0xfb0000 };
static int conflicts(uint64_t base, uint64_t size)
{
    return size > UINT64_MAX - base || (size && base < ram_end && base + size > ram);
}

static int layout_ok(const void *dt)
{
    uintptr_t addr = (uintptr_t)dt;
    if (addr < ram_end || addr > 0x7f000000 || (addr & 7) || fdt_check_header(dt)) return 0;
    unsigned bytes = fdt_totalsize(dt);
    if (bytes > 0x100000 || fdt_check_full(dt, bytes)) return 0;
    int chosen = fdt_path_offset(dt, "/chosen"), len;
    const char *video = fdt_getprop(dt, chosen, "atag,videolfb", &len);
    const uint32_t *tag = (const uint32_t *)video;
    const char panel[] = "mipi_mot_vid_djn_nt36525b_a_hdp_652";
    if (!video || len < 20 + (int)sizeof(panel) || tag[0] != fb || tag[1] ||
        tag[2] != 1 || tag[4] != pool || memcmp(video+20, panel, sizeof(panel))) return 0;
    const fdt32_t *start = fdt_getprop(dt, chosen, "linux,initrd-start", &len);
    if (!start || len != 4 || fdt32_to_cpu(*start) < ram_end) return 0;
    int reserved = fdt_path_offset(dt, "/reserved-memory");
    if (reserved < 0 || fdt_address_cells(dt,reserved) != 2 || fdt_size_cells(dt,reserved) != 2) return 0;
    int memory = fdt_path_offset(dt,"/memory");
    const fdt64_t *reg = fdt_getprop(dt,memory,"reg",&len);
    if (!reg || len != 16 || fdt64_ld(reg) != ram || fdt64_ld(reg+1) != 0xc0000000u) return 0;
    int match = 0, node;
    fdt_for_each_subnode(node,dt,reserved) {
        reg = fdt_getprop(dt,node,"reg",&len);
        if (!reg) continue;  // size-only Linux allocation requests have no fixed address
        if (len <= 0 || len % 16) return 0;
        for (int offset=0; offset<len/8; offset+=2) {
            uint64_t base=fdt64_ld(reg+offset), size=fdt64_ld(reg+offset+1);
            if (conflicts(base,size)) return 0;
            if (base == fb && size == pool && !strcmp(fdt_get_name(dt,node,0),"mblock-16-framebuffer")) match=1;
        }
    }
    for (int i=0; i<fdt_num_mem_rsv(dt); ++i) {
        uint64_t base,size;
        if (fdt_get_mem_rsv(dt,i,&base,&size) || conflicts(base,size)) return 0;
    }
    return match;
}

void prepare(const void *dt)
{
    if (!layout_ok(dt)) return;
    volatile uint32_t *pixels=(volatile uint32_t *)fb;
    for (unsigned y=32; y<64; ++y)
        for (unsigned x=0; x<720; ++x) pixels[y*736+x]=0xffffffff;
    memcpy((void *)0x43100000,kernel,kernel_end-kernel);
    memset((void *)0x40200000,0,ROOT_BYTES);
    memcpy((void *)0x40200000,root,root_end-root);
    memcpy((void *)0x43000000,trampoline,trampoline_end-trampoline);
    uint32_t *pd=(uint32_t *)0x43004000;
    memset(pd,0,16384);
    for (unsigned i=0; i<64; ++i) {
        uint32_t section=(ram+i*0x100000) | 0x1402; // privileged, normal noncacheable
        pd[0x400+i]=section;
        pd[0xe00+i]=section;
    }
    __asm__ volatile("dsb sy\n mov r0,#0\n mcr p15,0,r0,c7,c5,0\n dsb sy\n isb" ::: "r0","memory");
    ((void (*)(void))0x43000000)();
}
