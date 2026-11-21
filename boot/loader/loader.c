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

#ifdef VIOLET_MOTO
// The inherited watchdog remains armed while caches are off and the boot
// image is being staged. Reload it between bounded copies; keep its policy.
extern unsigned volume_load(void *destination);
extern unsigned record_boot_stage(unsigned char stage);
unsigned ticks_per_ms;
unsigned ticks(void) {
    unsigned lo, hi;
    __asm__ volatile("isb; mrrc p15,1,%0,%1,c14" : "=r"(lo), "=r"(hi));
    return lo;
}
static void show_previous(unsigned stage) {
    // LAST and a large seven-segment digit, visible before any app or USB starts.
    static const unsigned char letters[][5] = {{4,4,4,4,7},{2,5,7,5,5},{7,4,7,1,7},{7,2,2,2,2}};
    static const unsigned char digits[] = {0x3f,6,0x5b,0x4f,0x66,0x6d,0x7d};
    if (!stage || stage >= sizeof(digits)) return;
    volatile unsigned *p = (void *)fb;
    for (unsigned y=0;y<58;y++) for (unsigned x=0;x<212;x++) {
        unsigned on=0;
        if (x<144 && y>=8 && y<48 && x%36<24)
            on=letters[x/36][(y-8)/8] & (4u >> ((x%36)/8));
        if (x>=158 && x<192 && y>=5 && y<54) {
            unsigned a=x-158,b=y-5,m=digits[stage];
            if (a>=5 && a<29) on |= m & (b<5?1:b>=44?8:b>=22 && b<27?64:0);
            if (b>=5 && b<44) on |= m & (a<5?(b<24?32:16):a>=29?(b<24?2:4):0);
        }
        p[(240+y)*720+64+x]=on?0xffffffff:0xff101010;
    }
    __asm__ volatile("dsb sy" ::: "memory");
}
static void boot_progress(void) {
    volatile uint32_t *wdt = (void *)0x10007000;
    if (wdt[0] & 1u) wdt[2] = 0x1971;
}
static void stage_ram(uint32_t *dest, const uint32_t *src, unsigned bytes) {
    while (bytes >= 4) {
        unsigned words = bytes / 4;
        if (words > 4096) words = 4096;
        boot_progress();
        bytes -= words * 4;
        while (words--) *dest++ = src ? *src++ : 0;
    }
    if (bytes) {
        if (src) memcpy(dest, src, bytes);
        else memset(dest, 0, bytes);
    }
}
static int boot_token(const char *s,int n,const char *token) {
    unsigned k=strlen(token);
    for (int i=0;i+(int)k<n;i++) {
        if (i && s[i-1]!=' ' && s[i-1]!='\n') continue;
        if (!memcmp(s+i,token,k) &&
            (s[i+k]==' ' || s[i+k]=='\n' || !s[i+k])) return 1;
    }
    return 0;
}
static unsigned radio_profile(const void *dt) {
    int chosen=fdt_path_offset(dt,"/chosen"),len;
    const char *props[]={"mmi,bootconfig","bootargs"};
    for (unsigned i=0;i<2;i++) {
        const char *s=fdt_getprop(dt,chosen,props[i],&len);
        if (!s || !boot_token(s,len,"androidboot.device=ellis")) continue;
        int ca=boot_token(s,len,"androidboot.radio=CA"), nca=boot_token(s,len,"androidboot.radio=NCA");
        if (ca!=nca) return ca ? 2 : 1;
    }
    return 0;
}
#endif
void prepare(const void *dt)
{
    if (!layout_ok(dt)) return;
#ifdef VIOLET_MOTO
    unsigned hz;
    __asm__ volatile("mrc p15,0,%0,c14,c0,0" : "=r"(hz));
    ticks_per_ms=hz/1000;
    // Resume the inherited eMMC clock before opening the validated history area.
    *(volatile uint32_t *)0x1000108c = 1u << 2;
    *(volatile uint32_t *)0x100010c4 = 1u << 9;
    __asm__ volatile("dsb sy" ::: "memory");
    boot_progress();
    unsigned previous=record_boot_stage(1);
    show_previous(previous);
    if (previous >= 1 && previous <= 5) {
        // Make a failed boot's disk record readable even in a rapid reboot loop.
        unsigned start = ticks();
        while (ticks() - start < 8000u*ticks_per_ms) boot_progress();
    }
    extern char radio_firmware[], radio_firmware_end[];
    unsigned length = radio_firmware_end - radio_firmware;
    if (length > 0x1fffc0) return;
    stage_ram((void *)0x43e00000, 0, 0x200000);
    stage_ram((void *)0x43e00000, (const void *)radio_firmware, length);
    unsigned profile=radio_profile(dt);
    *(volatile unsigned *)0x43fffff8 = profile;
    if (previous != UINT32_MAX) record_boot_stage(2);
    *(volatile unsigned *)0x43fffff4 = previous;
    *(volatile unsigned *)0x43dffffc = volume_load((void *)0x43c00000);
    // Resume the bootloader's USB peripheral clock for the diagnostic device.
    *(volatile uint32_t *)0x10000098 = (1u << 23) | (1u << 16);
    *(volatile uint32_t *)0x10000094 = 1u << 16;
    *(volatile uint32_t *)0x10000004 = 1u << 22;
    *(volatile uint32_t *)0x10001084 = 1u << 8;
    __asm__ volatile("dsb sy" ::: "memory");
#endif
    volatile uint32_t *pixels=(volatile uint32_t *)fb;
    for (unsigned y=32; y<64; ++y)
        for (unsigned x=0; x<720; ++x) pixels[y*720+x]=0xffffffff;
#ifdef VIOLET_MOTO
    stage_ram((void *)0x43100000,(const void *)kernel,kernel_end-kernel);
    stage_ram((void *)0x40200000,0,ROOT_BYTES);
    stage_ram((void *)0x40200000,(const void *)root,root_end-root);
    boot_progress();
#else
    memcpy((void *)0x43100000,kernel,kernel_end-kernel);
    memset((void *)0x40200000,0,ROOT_BYTES);
    memcpy((void *)0x40200000,root,root_end-root);
#endif
    memcpy((void *)0x43000000,trampoline,trampoline_end-trampoline);
    uint32_t *pd=(uint32_t *)0x43004000;
    memset(pd,0,16384);
    for (unsigned i=0; i<64; ++i) {
        uint32_t section=(ram+i*0x100000) | 0x1402; // privileged, normal noncacheable
        pd[0x400+i]=section;
        pd[0xe00+i]=section;
    }
    __asm__ volatile("dsb sy\n mov r0,#0\n mcr p15,0,r0,c7,c5,0\n dsb sy\n isb" ::: "r0","memory");
#ifdef VIOLET_MOTO
    if (previous != UINT32_MAX) record_boot_stage(3);
    boot_progress();
#endif
    ((void (*)(void))0x43000000)();
}
