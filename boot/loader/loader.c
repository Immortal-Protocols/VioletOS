#include <libfdt.h>
#include <stdint.h>
#include <string.h>

extern char kernel[], kernel_end[], root[], root_end[], trampoline[], trampoline_end[];
enum { ram = 0x40000000, ram_end = 0x44000000, fb = 0x79450000, pool = 0xfb0000 };
static int conflicts(uint64_t base, uint64_t size)
{
    return size > UINT64_MAX - base || (size && base < ram_end && base + size > ram);
}

// Index into panels[] of the panel the bootloader lit; passed on for touch firmware.
static unsigned panel_index;
// Open apps get 0xc0000000-0xdfffffff (their file system image, then a 256 MiB
// page pool) only if no live reservation touches it. On the measured handset
// the modem, SCP and ATF reservations all lie below 0xc0000000.
enum { open_base = 0xc0000000u, open_size = 0x20000000u };
// The camera writes frames to 4 MiB at 0xe0000000 (services/camera), likewise
// only if no reservation touches it.
enum { camera_base = 0xe0000000u, camera_size = 0x400000u };
static int open_memory_ok = 1, camera_memory_ok = 1;
static uint32_t initrd_start, initrd_end, open_conflicts;
static uint32_t open_conflict[2];
static void check_open(uint64_t base, uint64_t size)
{
    if (size && base < (uint64_t)open_base + open_size && base + size > open_base) {
        if (open_memory_ok) open_conflict[0] = (uint32_t)base, open_conflict[1] = (uint32_t)size;
        open_memory_ok = 0;
        open_conflicts++;
    }
    if (size && base < (uint64_t)camera_base + camera_size && base + size > camera_base)
        camera_memory_ok = 0;
}
// Which check of the handoff refused it (1-11, in order), for the boot history.
static unsigned layout_refused;
static int refuse(unsigned why) { layout_refused = why; return 0; }
static int layout_ok(const void *dt)
{
    uintptr_t addr = (uintptr_t)dt;
    if (addr < ram_end || addr > 0x7f000000 || (addr & 7) || fdt_check_header(dt)) return refuse(1);
    unsigned bytes = fdt_totalsize(dt);
    if (bytes > 0x100000 || fdt_check_full(dt, bytes)) return refuse(2);
    int chosen = fdt_path_offset(dt, "/chosen"), len;
    const char *video = fdt_getprop(dt, chosen, "atag,videolfb", &len);
    const uint32_t *tag = (const uint32_t *)video;
    // XT2163DL units ship with different 720x1600 panels; the stock kernel lists
    // these five. The bootloader lights any of them into the same framebuffer.
    static const char *const panels[] = {
        "mipi_mot_vid_djn_nt36525b_a_hdp_652", "mipi_mot_vid_djn_nt36525b_hdp_652",
        "mipi_mot_vid_tm_nt36672a_hdp_652", "mipi_mot_vid_tm_hx83112a_hdp_652",
        "mipi_mot_vid_txd_ili7806s_hdp_652"};
    if (!video || len < 20 || tag[0] != fb || tag[1] || tag[2] != 1 || tag[4] != pool) return refuse(3);
    int known = 0;
    for (unsigned i = 0; i < sizeof(panels) / sizeof(panels[0]); i++) {
        size_t n = strlen(panels[i]) + 1;
        if (len >= 20 + (int)n && !memcmp(video + 20, panels[i], n)) known = 1, panel_index = i;
    }
    if (!known) return refuse(4);
    // The bootloader may write the start, as the end, as one or two cells.
    const fdt32_t *start = fdt_getprop(dt, chosen, "linux,initrd-start", &len);
    uint64_t first = !start ? 0 : len == 4 ? fdt32_to_cpu(*start) : len == 8 ? fdt64_ld((const fdt64_t *)start) : 0;
    if (first < ram_end || first > 0xffffffffu) return refuse(5);
    initrd_start = (uint32_t)first;
    // The bootloader may write the end as one or two cells.
    const fdt32_t *end = fdt_getprop(dt, chosen, "linux,initrd-end", &len);
    initrd_end = !end ? 0 : len == 4 ? fdt32_to_cpu(end[0]) : len == 8 ? (uint32_t)fdt64_ld((const fdt64_t *)end) : 0;
    int reserved = fdt_path_offset(dt, "/reserved-memory");
    if (reserved < 0 || fdt_address_cells(dt,reserved) != 2 || fdt_size_cells(dt,reserved) != 2) return refuse(6);
    int memory = fdt_path_offset(dt,"/memory");
    const fdt64_t *reg = fdt_getprop(dt,memory,"reg",&len);
    if (!reg || len != 16 || fdt64_ld(reg) != ram || fdt64_ld(reg+1) != 0xc0000000u) return refuse(7);
    int match = 0, node;
    fdt_for_each_subnode(node,dt,reserved) {
        reg = fdt_getprop(dt,node,"reg",&len);
        if (!reg) continue;  // size-only Linux allocation requests have no fixed address
        if (len <= 0 || len % 16) return refuse(8);
        for (int offset=0; offset<len/8; offset+=2) {
            uint64_t base=fdt64_ld(reg+offset), size=fdt64_ld(reg+offset+1);
            if (conflicts(base,size)) return refuse(9);
            check_open(base,size);
            if (base == fb && size == pool && !strcmp(fdt_get_name(dt,node,0),"mblock-16-framebuffer")) match=1;
        }
    }
    for (int i=0; i<fdt_num_mem_rsv(dt); ++i) {
        uint64_t base,size;
        if (fdt_get_mem_rsv(dt,i,&base,&size) || conflicts(base,size)) return refuse(10);
        check_open(base,size);
    }
    return match ? 1 : refuse(11);
}

#ifdef VIOLET_MOTO
// Boot history stages (targets/moto-g-pure/README.md): 1-3 from the AArch32
// loader, 21-23 from the AArch64 one; the supervisor and Storage add 4-6.
#ifdef __aarch64__
#define LOADER_STAGE(n) (20 + (n))
#else
#define LOADER_STAGE(n) (n)
#endif
// The inherited watchdog remains armed while caches are off and the boot
// image is being staged. Reload it between bounded copies; keep its policy.
extern unsigned volume_load(void *destination);
extern unsigned record_boot_stage(unsigned char stage);
unsigned ticks_per_ms;
unsigned ticks(void) {
#ifdef __aarch64__
    unsigned long count;
    __asm__ volatile("isb; mrs %0, cntpct_el0" : "=r"(count));
    return (unsigned)count;
#else
    unsigned lo, hi;
    __asm__ volatile("isb; mrrc p15,1,%0,%1,c14" : "=r"(lo), "=r"(hi));
    return lo;
#endif
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
#ifdef __aarch64__
// Bring-up evidence without a console (targets/moto-g-pure/README.md,
// "AArch64 boot path"): the MT6357 RTC fast-boot flag (RTC_PDN1 bit 13, as
// Power's set_fastboot and probe64/a64.S set it) and a TOPRGU reset, so LK
// stops in fastboot with "Reboot mode set to fastboot". A build with
// VIOLET_PROBE64_STAGE=N does this at stage N; the default build never does.
static volatile uint32_t *const pwrap = (void *)0x1000d000;
static void rgu_reset(void) {
    for (;;) {
        ((volatile uint32_t *)0x10007000)[0x14 / 4] = 0x1209;
        __asm__ volatile("dsb sy" ::: "memory");
    }
}
static unsigned pwrap_wait(unsigned want) {
    for (unsigned n = 0; n < 4000000; n++) {
        unsigned status = pwrap[0xc24 / 4], state = (status >> 16) & 7;
        if (state == want) return status;
        if (!want && state == 6) pwrap[0xc28 / 4] = 1;  // an uncollected read
    }
    rgu_reset();
    return 0;
}
static unsigned pmic_read(unsigned address) {
    pwrap_wait(0);
    pwrap[0xc20 / 4] = address << 15;
    unsigned value = pwrap_wait(6) & 0xffff;
    pwrap[0xc28 / 4] = 1;
    return value;
}
static void pmic_write(unsigned address, unsigned value) {
    pwrap_wait(0);
    pwrap[0xc20 / 4] = 0x80000000u | address << 15 | value;
    pwrap_wait(0);
}
static void rtc_commit(void) {
    pmic_write(0x5c2, 1);  // RTC_WRTGR
    for (unsigned n = 0; n < 500 && (pmic_read(0x588) & 0x40); n++) {  // RTC_BBPU busy
        unsigned start = ticks();
        boot_progress();
        while (ticks() - start < ticks_per_ms) {}
    }
}
static void fastboot_reset(void) {
    if ((pwrap[0xa0 / 4] & 1) && (pwrap[0xc24 / 4] & (1u << 22)) && (pmic_read(0x0a) >> 8) == 0x57) {
        pmic_write(0x5bc, 0x586a);  // RTC_PROT keys
        rtc_commit();
        pmic_write(0x5bc, 0x9136);
        rtc_commit();
        pmic_write(0x5b4, pmic_read(0x5b4) | 0x2000);  // RTC_PDN1 bit 13
        rtc_commit();
    }
    rgu_reset();
}
static void probe_stage(unsigned stage) {
#ifdef VIOLET_PROBE64_STAGE
    if (stage == VIOLET_PROBE64_STAGE) fastboot_reset();
#else
    (void)stage;
#endif
}
#endif
#endif
#ifdef VIOLET_VERIFIED_BOOT
// Verified boot (verified.rs): nothing is copied or started unless the boot
// manifest, signed by a key pinned in this loader, matches the kernel, the
// root task and, when the manifest binds it, the open apps' image.
struct images {
    const void *manifest; size_t manifest_bytes;
    const void *kernel; size_t kernel_bytes;
    const void *root; size_t root_bytes;
    const void *apps; size_t apps_room;
    unsigned floor[3];
};
extern unsigned verified_boot(const struct images *images, unsigned *version);
extern char boot_manifest[], boot_manifest_end[];
#ifdef VIOLET_MOTO
// The AArch64 handset (loader64.S, targets/moto-g-pure/build64.py; the
// AArch32 handset loader carries no manifest). No console: a refusal is the
// red band and boot stage 90 + its reason (prepare()). LK keeps and hands over
// no rollback floor, so the handset's check has none (version 0 is still
// refused). The /apps image is the ramdisk where LK put it (layout_ok()).
static unsigned verified_refused;
static void say(const char *s) { (void)s; }
static void say_number(unsigned v) { (void)v; }
#else
static void say(const char *s) {
    volatile uint32_t *uart = (void *)0x09000000;  // QEMU's PL011
    while (*s) *uart = (unsigned char)*s++;
}
static void say_number(unsigned v) {
    char text[11], *p = text + 10;
    *p = 0;
    do *--p = '0' + v % 10; while (v /= 10);
    say(p);
}
#endif
static int verified(void) {
#ifdef VIOLET_MOTO
    const unsigned floor[3] = {0, 0, 0};
    const void *apps = (const void *)(uintptr_t)initrd_start;
    size_t apps_room = initrd_end > initrd_start ? initrd_end - initrd_start : 0;
    // Hashing with the data cache off, under the inherited watchdog.
    boot_progress();
#else
    // The bootloader's stored rollback floor (tools/boot_manifest.py FLOOR_ADDRESS).
    const volatile unsigned *floor = (void *)0x47870000;
    const void *apps = (void *)0x4a000000;
    size_t apps_room = 0x06000000;
#endif
    const struct images images = {
        boot_manifest, boot_manifest_end - boot_manifest, kernel, kernel_end - kernel,
        root, root_end - root, apps, apps_room, {floor[0], floor[1], floor[2]}};
    unsigned version = 0, why = verified_boot(&images, &version);
#ifdef VIOLET_MOTO
    boot_progress();
    verified_refused = why;
#endif
    if (!why) {
        say("VioletOS loader: verified boot, system version ");
        say_number(version);
        say("\n");
        return 1;
    }
    say("VioletOS loader: boot refused, reason ");
    say_number(why);
    say("\n");
    volatile uint32_t *pixels = (volatile uint32_t *)fb;
    for (unsigned y = 32; y < 64; ++y)
        for (unsigned x = 0; x < 720; ++x) pixels[y * 720 + x] = 0xffff0000;
    return 0;
}
#endif
#ifdef __aarch64__
// loader64.S: the trampoline, and the exception level LK entered at (CurrentEL).
extern char trampoline64[], trampoline64_end[], boot_tables[], boot_tables_end[];
extern unsigned entry_el;
#endif
void prepare(const void *dt)
{
#ifdef __aarch64__
    unsigned long hz0;
    __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(hz0));
    ticks_per_ms = (unsigned)hz0 / 1000;
    probe_stage(1);
    // Linux's arm64 protocol passes the device tree in x0. Should LK's 64-bit
    // path leave it elsewhere, the boot header's tags_addr is where it loads it.
    static unsigned dt_fallback;
    if (fdt_magic(dt) != FDT_MAGIC) dt = (const void *)0x47880000, dt_fallback = 0x100;
    if (!layout_ok(dt)) {
        // Say which check refused, as boot stage 40 + n, before halting (the
        // watchdog ends the try): the next 32-bit boot reports it over USB.
        *(volatile uint32_t *)0x1000108c = 1u << 2;
        *(volatile uint32_t *)0x100010c4 = 1u << 9;
        __asm__ volatile("dsb sy" ::: "memory");
        record_boot_stage(40 + layout_refused + (dt_fallback ? 20 : 0));
        return;
    }
    probe_stage(2);
#else
    if (!layout_ok(dt)) return;
#endif
#ifdef VIOLET_VERIFIED_BOOT
    if (!verified()) {
#ifdef VIOLET_MOTO
        // As a refused handoff: the eMMC clock on, then the stage.
        *(volatile uint32_t *)0x1000108c = 1u << 2;
        *(volatile uint32_t *)0x100010c4 = 1u << 9;
        __asm__ volatile("dsb sy" ::: "memory");
        record_boot_stage(90 + verified_refused);
#endif
        return;
    }
#endif
#ifdef VIOLET_MOTO
#ifdef __aarch64__
    unsigned long hz;
    __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(hz));
#else
    unsigned hz;
    __asm__ volatile("mrc p15,0,%0,c14,c0,0" : "=r"(hz));
#endif
    ticks_per_ms=hz/1000;
    // Resume the inherited eMMC clock before opening the validated history area.
    *(volatile uint32_t *)0x1000108c = 1u << 2;
    *(volatile uint32_t *)0x100010c4 = 1u << 9;
    __asm__ volatile("dsb sy" ::: "memory");
    boot_progress();
    // The AArch64 loader records its own stages (21-23): a 32-bit boot that
    // follows (slot A, after B's tries) reports the last as previous_boot_stage.
    unsigned previous=record_boot_stage(LOADER_STAGE(1));
#ifdef __aarch64__
    probe_stage(3);
#endif
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
    // "PAN" tag plus the panel index, so touch can tell a missing value from panel 0.
    *(volatile unsigned *)0x43fffff0 = 0x50414e00u | panel_index;
#ifdef __aarch64__
    // Why the history is unreadable, for USB Diagnostics (request 0x59).
    if (previous == UINT32_MAX) {
        extern void boot_disk_status(unsigned char *out);
        boot_disk_status((unsigned char *)0x43ffffc0);
    }
#endif
    if (previous != UINT32_MAX) record_boot_stage(LOADER_STAGE(2));
    *(volatile unsigned *)0x43fffff4 = previous;
    // The open apps' file system image, then "OPEN" for the supervisor if the
    // whole window is free.
    // The image is the boot image's ramdisk, wherever the bootloader put it.
    *(volatile unsigned *)0x43ffffec = 0;
    *(volatile unsigned *)0x43ffffe4 = open_conflict[0];
    *(volatile unsigned *)0x43ffffe0 = open_conflict[1];
    *(volatile unsigned *)0x43ffffdc = initrd_start;
    *(volatile unsigned *)0x43ffffd8 = initrd_end;
    if (open_memory_ok && initrd_end > initrd_start && initrd_end - initrd_start <= 0x06000000) {
        stage_ram((void *)(uintptr_t)open_base, (const void *)(uintptr_t)initrd_start, (initrd_end - initrd_start + 3) & ~3u);
        *(volatile unsigned *)0x43ffffe8 = initrd_end - initrd_start;
        *(volatile unsigned *)0x43ffffec = 0x4e45504f;
    }
    *(volatile unsigned *)0x43ffffd4 = camera_memory_ok ? 0x4d4d4143u : 0; // "CAMM"
#ifdef __aarch64__
    // "EL" and the level LK entered the kernel at (0x454c0002: EL2).
    *(volatile unsigned *)0x43ffffd0 = 0x454c0000u | dt_fallback | (entry_el >> 2);
#endif
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
#if defined(VIOLET_MOTO) && defined(__aarch64__)
    // AArch64 (loader64.S): the kernel is staged and moved into place by the
    // trampoline (its destination, 0x40000000 to the end of its zeroed data,
    // covers this image); the root task goes straight to its own window.
    stage_ram((void *)KERNEL_STAGE,(const void *)kernel,kernel_end-kernel);
    stage_ram((void *)ROOT_PA,0,ROOT_BYTES);
    stage_ram((void *)ROOT_PA,(const void *)root,root_end-root);
    stage_ram((void *)0x43000000,(const void *)trampoline64,trampoline64_end-trampoline64);
    stage_ram((void *)TABLES_PA,(const void *)boot_tables,boot_tables_end-boot_tables);
    if (previous != UINT32_MAX) record_boot_stage(LOADER_STAGE(3));
    boot_progress();
    probe_stage(4);
    ((void (*)(void))0x43000000)();
}
#else
#if defined(VIOLET_MOTO)
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
    if (previous != UINT32_MAX) record_boot_stage(LOADER_STAGE(3));
    boot_progress();
#endif
    ((void (*)(void))0x43000000)();
}
#endif
