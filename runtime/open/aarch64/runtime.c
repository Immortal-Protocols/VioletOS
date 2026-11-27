// 64-bit Open Runtime bootstrap for QEMU virt. Each guest runs in a separate
// seL4 address space, with an empty capability table. Only touched pages
// consume physical memory. The root holds a second mapping to copy guest I/O.
#include <sel4/sel4.h>
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

enum { PAGE = 4096, REGIONS = 1024, FRAMES = 655360, PAGE_BUCKETS = 65536 };
#define USER_END (seL4_UserVSpaceTop + 1ULL)
#define SIGNAL_GATE (USER_END - PAGE)
#define VDSO_BASE (SIGNAL_GATE - 2 * PAGE)
extern const unsigned char vdso_start[], vdso_end[];
extern unsigned char signal_gate_start[], signal_gate_end[];
#define STACK_TOP (USER_END - (1ULL << 30))
#define ALIAS     0x800000000UL
#define CROOT seL4_CapInitThreadCNode

static seL4_BootInfo *bi;
seL4_IPCBuffer *__sel4_ipc_buffer;
static seL4_CPtr preferred_untyped;
static seL4_CPtr slot, endpoint, response;
extern unsigned char guest_start[], guest_end[], argv_start[], argv_end[];
extern unsigned char __ehdr_start[], bundle_data[], bundle_end[];

static void say(const char *s) { while (*s) seL4_DebugPutChar(*s++); }
static void hex(uint64_t val) {
    char s[17];
    s[16] = 0;
    for (int i = 15; i >= 0; i--) { s[i] = "0123456789abcdef"[val & 15]; val >>= 4; }
    say(s);
}
static __attribute__((noreturn)) void halt(void) {
    for (;;) seL4_TCB_Suspend(seL4_CapInitThreadTCB);
}
static void check(const char *operation, seL4_Error err) {
    if (err != seL4_NoError) { say(operation); say(": "); hex(err); say("\n"); halt(); }
}
// The root task is built without SIMD. Copy aligned framebuffer, file and
// demand-paged data a machine word at a time on the guest's hot I/O paths.
typedef uint64_t copy_word __attribute__((__may_alias__));
void *memset(void *p, int c, size_t n) {
    unsigned char *dst = p;
    while (n && ((uintptr_t)dst & 7)) { *dst++ = c; --n; }
    uint64_t repeated = (uint8_t)c * UINT64_C(0x0101010101010101);
    while (n >= 8) { *(copy_word *)dst = repeated; dst += 8; n -= 8; }
    while (n--) *dst++ = c;
    return p;
}
void *memmove(void *dest, const void *src, size_t size) {
    unsigned char *out = dest;
    const unsigned char *in = src;
    if ((uintptr_t)out <= (uintptr_t)in) {
        for (size_t i = 0; i < size; ++i) out[i] = in[i];
    } else {
        while (size) { --size; out[size] = in[size]; }
    }
    return dest;
}
void *memcpy(void *p, const void *src, size_t n) {
    unsigned char *dst = p;
    const unsigned char *in = src;
    if (!(((uintptr_t)dst ^ (uintptr_t)in) & 7)) {
        while (n && ((uintptr_t)dst & 7)) { *dst++ = *in++; --n; }
        while (n >= 8) { *(copy_word *)dst = *(const copy_word *)in; dst += 8; in += 8; n -= 8; }
    }
    while (n--) *dst++ = *in++;
    return p;
}
void __assert_fail(const char *s, const char *file, int line, const char *fn) {
    (void)file; (void)line; (void)fn; say(s); halt();
}

#include "files.h"

struct entropy { unsigned char magic[8]; uint64_t epoch_ns; unsigned char bytes[4096]; };
volatile struct entropy boot_entropy;
static unsigned entropy_used;
static uint64_t start_ticks;
enum { ENTROPY_CAPACITY = 16384 };
struct entropy_channel { volatile uint32_t head, tail, online; unsigned char bytes[ENTROPY_CAPACITY]; };
static struct entropy_channel entropy_channel __attribute__((aligned(4096)));
static bool has_boot_entropy(void) {
    const char *magic = "VOSRAND1";
    for (unsigned i = 0; i < 8; i++) if (boot_entropy.magic[i] != magic[i]) return false;
    return true;
}
static size_t entropy_available(void) {
    size_t boot = has_boot_entropy() ? sizeof(boot_entropy.bytes) - entropy_used : 0;
    unsigned ready = __atomic_load_n(&entropy_channel.head, __ATOMIC_ACQUIRE) - entropy_channel.tail;
    return boot + (ready <= ENTROPY_CAPACITY ? ready : 0);
}
static bool random_bytes(unsigned char *dst, size_t len) {
    if (entropy_available() < len) return false;
    size_t boot = has_boot_entropy() ? sizeof(boot_entropy.bytes) - entropy_used : 0;
    while (len && boot--) {
        *dst++ = boot_entropy.bytes[entropy_used];
        boot_entropy.bytes[entropy_used++] = 0; len--;
    }
    uint32_t tail = entropy_channel.tail;
    while (len--) {
        unsigned idx = tail++ % ENTROPY_CAPACITY;
        *dst++ = entropy_channel.bytes[idx]; entropy_channel.bytes[idx] = 0;
    }
    __atomic_store_n(&entropy_channel.tail, tail, __ATOMIC_RELEASE);
    return true;
}
static seL4_CPtr alloc_object(seL4_Word kind, unsigned bits) {
    if (slot >= bi->empty.end) { say("out of cap slots\n"); halt(); }
    if (preferred_untyped) {
        int err = seL4_Untyped_Retype(preferred_untyped, kind, bits, CROOT, 0, 0, slot, 1);
        if (err == seL4_NoError) return slot++;
        if (err != seL4_NotEnoughMemory) check("retype", err);
    }
    for (seL4_CPtr u = bi->untyped.start; u < bi->untyped.end; u++) {
        if (bi->untypedList[u - bi->untyped.start].isDevice) continue;
        int err = seL4_Untyped_Retype(u, kind, bits, CROOT, 0, 0, slot, 1);
        if (err == seL4_NoError) return slot++;
        if (err != seL4_NotEnoughMemory) check("retype", err);
    }
    say("out of untyped memory\n"); halt();
}
static void install_table(seL4_CPtr target, uintptr_t va);
static void map(seL4_CPtr frame, seL4_CPtr target, uintptr_t va, int prot) {
    seL4_CapRights_t rights = seL4_CapRights_new(0, 0, 1, !!(prot & 2));
    int attrs = seL4_ARM_Default_VMAttributes | ((prot & 4) ? 0 : seL4_ARM_ExecuteNever);
    int err;
    while ((err = seL4_ARM_Page_Map(frame, target, va, rights, attrs)) == seL4_FailedLookup) {
        install_table(target, va);
    }
    check("page map", err);
}

#include "tmpfs.h"

struct region {
    struct tmp_node *tmp;
    uint64_t tmp_base, tmp_offset;
    bool tmp_shared;
    uintptr_t lo, hi;
    int prot;
    const unsigned char *data;
    bool shared_readonly;
    uintptr_t data_lo, data_hi; // file-backed interval; other bytes begin zeroed
};
enum { PROCESS_LIMIT = 32 };
struct vm_space {
    seL4_CPtr cap;
    struct region areas[REGIONS];
    unsigned count, users, table_count;
    seL4_CPtr tables[4096];
    uintptr_t next, brk;
};
struct process {
    int pid, parent, creator_tid, pgid, sid;
    unsigned parent_signal;
    int vfork_tid; // the parent thread waiting for exec or exit
    bool zombie, executed;
    unsigned status, death_signal;
    struct vm_space *vm;
    const struct file *image;
    struct fd_table files;
    unsigned char actions[65][32];
};
static struct process processes[PROCESS_LIMIT], *active_process;
static struct vm_space spaces[PROCESS_LIMIT + 1], *vm; // spare for atomic exec
static void activate(struct process *p) {
    active_process = p; vm = p->vm; file_table = &p->files;
}
#define regions (vm->areas)
#define nregions (vm->count)
#define next_map (vm->next)
#define space (vm->cap)
static bool tmp_mapped(struct tmp_node *n) {
    for (unsigned i = 0; i < PROCESS_LIMIT + 1; i++) if (spaces[i].users)
        for (unsigned j = 0; j < spaces[i].count; j++) if (spaces[i].areas[j].tmp == n) return true;
    return false;
}
struct page { struct tmp_block *backing; const unsigned char *packed; seL4_CPtr shared_slot; uintptr_t va; seL4_CPtr cap; bool mapped; struct vm_space *owner; unsigned next; };
static seL4_CPtr frame_cap(struct page *p) { return p->backing || p->packed ? p->shared_slot : p->cap; }
static struct region scratch[REGIONS];
static struct page pages[FRAMES];
static unsigned npages, live_pages, free_page, page_buckets[PAGE_BUCKETS];
static unsigned packed_pages;
// Links are one-based indices so an empty bucket and a free-list tail are zero.
static unsigned page_bucket(const struct vm_space *v, uintptr_t va) {
    uint64_t key = (va / PAGE) ^ ((uint64_t)(v - spaces) << 27);
    key ^= key >> 21;
    return (key * 0x9e3779b97f4a7c15ULL) >> 48;
}
static void forget_page(struct page *p) {
    if (p->mapped) check("page unmap", seL4_ARM_Page_Unmap(frame_cap(p)));
    if (p->backing || p->packed) check("shared page delete", seL4_CNode_Delete(CROOT, p->shared_slot, 64));
    if (p->packed) --packed_pages;
    p->backing = NULL; p->packed = NULL;
    unsigned *link = &page_buckets[page_bucket(p->owner, p->va)];
    unsigned index = (unsigned)(p - pages) + 1;
    while (*link != index) link = &pages[*link - 1].next;
    *link = p->next;
    p->va = 0; p->owner = NULL; p->mapped = false;
    p->next = free_page; free_page = index;
    live_pages--;
}
static seL4_CPtr free_tables[16384];
static unsigned free_table_count;
static void install_table(seL4_CPtr target, uintptr_t va) {
    seL4_CPtr pt = free_table_count ? free_tables[--free_table_count] : alloc_object(seL4_ARM_PageTableObject, 0);
    if (target != seL4_CapInitThreadVSpace) {
        if (vm->table_count == sizeof(vm->tables) / sizeof(*vm->tables)) {
            say("address space exhausted page tables\n"); halt();
        }
        vm->tables[vm->table_count++] = pt;
    }
    check("page table map", seL4_ARM_PageTable_Map(pt, target, va, seL4_ARM_Default_VMAttributes));
}
static uint64_t up(uint64_t x) { return (x + PAGE - 1) & -(uint64_t)PAGE; }
static struct region *region_at(uintptr_t va) {
    for (unsigned i = 0; i < nregions; i++)
        if (regions[i].lo <= va && va < regions[i].hi) return &regions[i];
    return NULL;
}

static uintptr_t gap_after(uintptr_t lo, uintptr_t len) {
    for (;;) {
        if (lo >= USER_END || len > USER_END - lo) return 0;
        unsigned i = 0;
        for (; i < nregions; i++) {
            if (regions[i].lo < lo + len && lo < regions[i].hi) {
                lo = regions[i].hi;
                break;
            }
        }
        if (i == nregions) return lo;
    }
}

// Repeated mprotect and brk calls can split one anonymous arena into many
// adjacent regions. Rejoin anonymous regions with identical access rights.
static void compact_regions(void) {
    for (unsigned i = 1; i < nregions; i++) {
        struct region current = regions[i];
        unsigned j = i;
        while (j && regions[j-1].lo > current.lo) {
            regions[j] = regions[j-1];
            --j;
        }
        regions[j] = current;
    }
    unsigned used = 0;
    for (unsigned i = 0; i < nregions; i++) {
        struct region *r = &regions[i];
        if (used) {
            struct region *last = &regions[used - 1];
            if (last->hi == r->lo && last->prot == r->prot && !last->tmp && !r->tmp
                && !last->data && !r->data && !last->shared_readonly && !r->shared_readonly) {
                last->hi = r->hi;
                continue;
            }
        }
        regions[used++] = *r;
    }
    nregions = used;
}

static bool add_region(uintptr_t lo, uintptr_t hi, int prot) {
    if (nregions == REGIONS) {
        compact_regions();
        if (nregions == REGIONS) { say("virtual mapping limit in "); say(active_process->image->name); say("\n"); }
    }
    if (!lo || lo >= hi || hi > USER_END || nregions >= REGIONS) return false;
    for (unsigned i = 0; i < nregions; i++)
        if (regions[i].lo < hi && lo < regions[i].hi) return false;
    regions[nregions++] = (struct region){ .lo=lo, .hi=hi, .prot=prot };
    return true;
}

static struct page *resident_page(uintptr_t va) {
    unsigned bucket = page_bucket(vm, va);
    for (unsigned link = page_buckets[bucket]; link; link = pages[link - 1].next) {
        struct page *p = &pages[link - 1];
        if (p->owner == vm && p->va == va) return p;
    }
    return NULL;
}

static unsigned char *private_bytes(struct page *p) {
    uintptr_t alias = ALIAS + (p - pages) * PAGE;
    if (!p->cap) {
        p->cap = alloc_object(seL4_ARM_SmallPageObject, 0);
        seL4_CPtr copy = slot++;
        check("frame copy", seL4_CNode_Copy(CROOT, copy, 64, CROOT, p->cap, 64, seL4_AllRights));
        map(copy, seL4_CapInitThreadVSpace, alias, 3);
    }
    return (unsigned char *)alias;
}

// Only a whole aligned page belonging to an immutable guest file may be lent.
// The source image is already resident in the root task. Each borrower gets a
// distinct read-only capability. Boundary pages retain their zero-filled padding.
static const unsigned char *packed_source(const struct region *r, uintptr_t va) {
    if (!r->data || (r->prot & 2) || va < r->data_lo || va + PAGE > r->data_hi) return NULL;
    uintptr_t addr = (uintptr_t)r->data + va - r->data_lo;
    if (addr % PAGE || addr < (uintptr_t)bundle_data || addr + PAGE > (uintptr_t)bundle_end) return NULL;
    return (const unsigned char *)addr;
}
static void privatize_packed(struct page *p) {
    if (p->mapped) { check("unmap packed page", seL4_ARM_Page_Unmap(p->shared_slot)); p->mapped = false; }
    memcpy(private_bytes(p), p->packed, PAGE);
    check("drop packed page cap", seL4_CNode_Delete(CROOT, p->shared_slot, 64));
    p->packed = NULL;
    --packed_pages;
}
static struct page *page_at(uintptr_t va, const struct region *r) {
    int prot = r->prot;
    va &= -(uintptr_t)PAGE;
    struct page *resident = resident_page(va);
    if (resident) return resident;
    unsigned bucket = page_bucket(vm, va);
    uint64_t offset = r->tmp_offset + va - r->tmp_base;
    struct tmp_block *block = NULL;
    if (r->tmp) {
        if (offset >= r->tmp->file.size) return NULL;
        block = tmp_page(r->tmp, offset, r->tmp_shared);
        if (r->tmp_shared && !block) return NULL;
    }
    struct page *p;
    if (free_page) { p = &pages[free_page - 1]; free_page = p->next; }
    else {
        if (npages == FRAMES) return NULL;
        p = &pages[npages++];
    }
    const unsigned char *packed = packed_source(r, va);
    if (r->tmp_shared) {
        if (!p->shared_slot) p->shared_slot = slot++;
        check("shared file cap", seL4_CNode_Copy(CROOT, p->shared_slot, 64, CROOT, block->cap, 64, seL4_AllRights));
        p->backing = block;
    } else if (packed) {
        seL4_CPtr source = bi->userImageFrames.start + ((uintptr_t)packed - (uintptr_t)__ehdr_start) / PAGE;
        if (source >= bi->userImageFrames.end) { say("file frame outside boot image\n"); halt(); }
        if (!p->shared_slot) p->shared_slot = slot++;
        check("borrow packed page", seL4_CNode_Copy(CROOT, p->shared_slot, 64, CROOT, source, 64,
                                                    seL4_CapRights_new(0, 0, 1, 0)));
        p->packed = packed;
        ++packed_pages;
    } else {
        unsigned char *dest = private_bytes(p);
        memset(dest, 0, PAGE);
        if (r->data && r->data_hi > va && r->data_lo < va + PAGE) {
            uintptr_t lo = va > r->data_lo ? va : r->data_lo;
            uintptr_t hi = va + PAGE < r->data_hi ? va + PAGE : r->data_hi;
            memcpy(dest + lo - va, r->data + lo - r->data_lo, hi - lo);
        }
        if (block) memcpy(dest, tmp_bytes(block), PAGE);
    }
    p->va = va; p->owner = vm;
    p->next = page_buckets[bucket]; page_buckets[bucket] = (unsigned)(p - pages) + 1;
    live_pages++;
    if (prot) { map(frame_cap(p), space, va, prot); p->mapped = true; }
    if (prot & 4) check("sync code", seL4_ARM_Page_Unify_Instruction(frame_cap(p), 0, PAGE));
    return p;
}

static unsigned char *guest_ptr(uintptr_t va, int needed) {
    struct region *r = region_at(va);
    if (!r || (r->prot & needed) != needed) return NULL;
    struct page *p = page_at(va, r);
    if (p && p->backing) return tmp_bytes(p->backing) + va % PAGE;
    if (p && p->packed) return (unsigned char *)p->packed + va % PAGE;
    return p ? (unsigned char *)(ALIAS + (p - pages) * PAGE + va % PAGE) : NULL;
}
static bool put(uintptr_t dest, const void *src, size_t n) {
    while (n) {
        size_t len = PAGE - dest % PAGE;
        if (len > n) len = n;
        unsigned char *p = guest_ptr(dest, 2); // construct the initial stack
        if (!p) return false;
        memcpy(p, src, len);
        src = (const char *)src + len; dest += len; n -= len;
    }
    return true;
}
static void change_page(struct page *p, int prot) {
    if (prot < 0) { forget_page(p); return; }
    if (p->mapped) { check("unmap", seL4_ARM_Page_Unmap(frame_cap(p))); p->mapped = false; }
    if ((prot & 2) && p->packed) privatize_packed(p);
    if (prot) { map(frame_cap(p), space, p->va, prot); p->mapped = true; }
}

static void update_pages(uintptr_t start, uintptr_t end, int prot) {
    if ((end - start) / PAGE < npages) {
        for (uintptr_t va = start; va < end; va += PAGE) {
            struct page *p = resident_page(va);
            if (p) change_page(p, prot);
        }
    } else {
        for (unsigned i = 0; i < npages; i++) {
            struct page *p = &pages[i];
            if (p->owner == vm && p->va >= start && p->va < end) change_page(p, prot);
        }
    }
}

static long change(uintptr_t addr, uint64_t len, int prot, bool remove) {
    if (addr % PAGE || !len || addr >= USER_END || len > USER_END - addr || (prot & ~7)) return -22;
    uintptr_t end = up(addr + len);
    unsigned used = 0;
    uint64_t covered = 0;
    if (nregions >= REGIONS - 3) compact_regions();
    for (unsigned i = 0; i < nregions; i++) {
        struct region r = regions[i];
        if (used == REGIONS) return -12;
        if (r.hi <= addr || end <= r.lo) { scratch[used++] = r; continue; }
        if (!remove && (prot & 2) && r.shared_readonly) return -13;
        uintptr_t l = r.lo > addr ? r.lo : addr, h = r.hi < end ? r.hi : end;
        covered += h - l;
        if (!remove && prot == r.prot) { scratch[used++] = r; continue; }
        unsigned pieces = (r.lo < l) + !remove + (h < r.hi);
        if (pieces > REGIONS - used) return -12;
        if (r.lo < l) { scratch[used] = r; scratch[used++].hi = l; }
        if (!remove) { scratch[used] = r; scratch[used].lo = l; scratch[used].hi = h; scratch[used++].prot = prot; }
        if (h < r.hi) { scratch[used] = r; scratch[used++].lo = h; }
    }
    if (!remove && covered != end - addr) return -12;
    memcpy(regions, scratch, used * sizeof(*regions)); nregions = used;
    update_pages(addr, end, remove ? -1 : prot);
    return 0;
}

// Discard resident frames while preserving the mapping and its backing file.
// The next fault recreates anonymous pages zeroed and file pages from their source.
static long advise_pages(uintptr_t start, uint64_t len, uint64_t advice) {
    if (advice != 4 && advice != 8) return -22; // MADV_DONTNEED, MADV_FREE
    if (start % PAGE || start >= USER_END || len > USER_END - start) return -22;
    uintptr_t end = up(start + len);
    for (uintptr_t pos = start; pos < end;) {
        struct region *r = region_at(pos);
        if (!r) return -12;
        if (advice == 8 && (r->tmp || r->data || r->shared_readonly)) return -22;
        pos = r->hi;
    }
    if (advice == 8) return 0; // Linux may retain lazy-free pages until memory pressure.
    update_pages(start, end, -1);
    return 0;
}

struct elf_header {
    char id[16]; uint16_t type, machine; uint32_t version;
    uint64_t entry, phoff, shoff; uint32_t flags; uint16_t ehsize, phsize, phnum, shsize, shnum, shstr;
};
struct phdr { uint32_t type, flags; uint64_t offset, va, pa, size, memsz, align; };
struct elf_info { uint64_t entry, phva, base; uint16_t phsize, phnum; const struct file *interp; };
static bool land_exec(const struct file *);
static bool load_elf(const struct file *file, uintptr_t bias, struct elf_info *info) {
    if (!land_exec(file)) return false;
    size_t size = file->size;
    if (size < sizeof(struct elf_header)) return false;
    const struct elf_header *h = (const void *)file->data;
    if (h->id[0] != 127 || h->id[1] != 'E' || h->id[2] != 'L' || h->id[3] != 'F' ||
        h->id[4] != 2 || h->id[5] != 1 || h->machine != 183 || (h->type != 2 && h->type != 3) ||
        h->phsize != sizeof(struct phdr) || h->phoff > size ||
        h->phnum > (size - h->phoff) / sizeof(struct phdr)) return false;
    const struct phdr *ph = (const void *)(file->data + h->phoff);
    if (h->type == 2) bias = 0;
    uint64_t phva = 0;
    for (unsigned i = 0; i < h->phnum; i++) {
        if (ph[i].type == 3) {
            uint64_t at = ph[i].offset, n = ph[i].size;
            if (!n || at > size || n > size - at || file->data[at + n - 1]) return false;
            info->interp = find_file((const char *)file->data + at);
            if (!info->interp) { say("missing ELF interpreter "); say((const char *)file->data + at); say("\n"); return false; }
        }
        if (ph[i].type != 1 || !ph[i].memsz) continue;
        if (ph[i].va > USER_END - bias) return false;
        uint64_t va = bias + ph[i].va, off = ph[i].offset, bytes = ph[i].size;
        if (va >= USER_END || ph[i].memsz > USER_END - va || bytes > ph[i].memsz ||
            off > size || bytes > size - off) return false;
        int prot = ((ph[i].flags & 4) >> 2) | (ph[i].flags & 2) | ((ph[i].flags & 1) << 2);
        if (!add_region(va & -(uintptr_t)PAGE, up(va + ph[i].memsz), prot)) return false;
        struct region *region = &regions[nregions - 1];
        region->data = file->data + off; region->data_lo = va; region->data_hi = va + bytes;
        if (off <= h->phoff && h->phoff - off < bytes) phva = va + h->phoff - off;
    }
    info->entry = bias + h->entry; info->base = bias;
    info->phva = phva; info->phsize = h->phsize; info->phnum = h->phnum;
    return region_at(info->entry) && (region_at(info->entry)->prot & 4);
}
static bool load(seL4_UserContext *ctx, const struct file *file, const unsigned char *text,
                 size_t arg_bytes, size_t env_bytes) {
    struct elf_info app = {0}, ld = {0};
    if (!load_elf(file, 0x4000000UL, &app)) return false;
    if (app.interp && !load_elf(app.interp, 0x40000000UL, &ld)) return false;
    if (vdso_end - vdso_start > 2 * PAGE || !add_region(VDSO_BASE, SIGNAL_GATE, 5)) return false;
    struct region *vdso = &regions[nregions - 1];
    vdso->data = vdso_start; vdso->data_lo = VDSO_BASE; vdso->data_hi = VDSO_BASE + (vdso_end - vdso_start);
    uint64_t clock_base[] = {start_ticks, boot_entropy.epoch_ns};
    // The guest sees the vDSO as RX for its lifetime. Seed its private first
    // page through the root task's writable alias before starting the thread.
    struct page *clock_page = page_at(VDSO_BASE, vdso);
    if (!clock_page || clock_page->packed || clock_page->backing) return false;
    memcpy(private_bytes(clock_page) + PAGE - sizeof(clock_base), clock_base, sizeof(clock_base));
    if (!add_region(SIGNAL_GATE, SIGNAL_GATE + PAGE, 5)) return false;
    struct region *gate = &regions[nregions - 1];
    gate->data = signal_gate_start; gate->data_lo = SIGNAL_GATE;
    gate->data_hi = SIGNAL_GATE + (signal_gate_end - signal_gate_start);
    if (!add_region(STACK_TOP - (8 << 20), STACK_TOP, 3)) return false;
    size_t strings_len = arg_bytes + env_bytes;
    uintptr_t strings = STACK_TOP - strings_len;
    if (!put(strings, text, strings_len)) return false;
    uintptr_t words[296] = {0};
    size_t used = 1;
    for (size_t i = 0; i < arg_bytes; i++) {
        if (!i || !text[i - 1]) {
            if (used > 128) return false;
            words[used++] = strings + i;
        }
    }
    words[0] = used - 1;
    words[used++] = 0; // argv terminator
    for (size_t i = arg_bytes; i < strings_len; i++) {
        if (i == arg_bytes || !text[i-1]) {
            if (used > 258) return false;
            words[used++] = strings + i;
        }
    }
    words[used++] = 0;
    unsigned char random[16] = {0};
    uintptr_t rand_addr = strings - 16;
    bool seeded = random_bytes(random, sizeof(random));
    if (seeded && !put(rand_addr, random, sizeof(random))) return false;
    // AT_HWCAP: Cortex-A57 FPU/ASIMD; use a conservative common hardware set.
    uintptr_t aux[] = {3, app.phva, 4, app.phsize, 5, app.phnum, 6, PAGE, 7, ld.base,
        9, app.entry, 11, 1000, 12, 1000, 13, 1000, 14, 1000, 16, 3, 17, 100,
        23, 0, 25, seeded ? rand_addr : 0, 31, strings, 33, VDSO_BASE, 0, 0};
    memcpy(words + used, aux, sizeof(aux));
    size_t total = used * sizeof(*words) + sizeof(aux);
    ctx->sp = (rand_addr - total) & -(uintptr_t)16;
    ctx->pc = app.interp ? ld.entry : app.entry;
    return put(ctx->sp, words, total);
}

static bool transfer(uintptr_t addr, void *buf, size_t len, bool output) {
    unsigned char *bytes = buf;
    while (len) {
        size_t n = PAGE - addr % PAGE;
        if (n > len) n = len;
        unsigned char *p = guest_ptr(addr, output ? 2 : 1);
        if (!p) return false;
        if (output) memcpy(p, bytes, n); else memcpy(bytes, p, n);
        addr += n; bytes += n; len -= n;
    }
    return true;
}

#include "threads.h"
#include "display.h"
#include "audio.h"
#ifdef VIOLET_NETWORK
#include "inet.h"
#endif
#include "fileops.h"
#include "locks.h"
#include "process.h"
#include "groups.h"
static void read_full_context(seL4_UserContext *r) {
    check("registers", seL4_TCB_ReadRegisters(caller->tcb, 0, 0, sizeof(*r) / sizeof(long), r));
}
#include "seccomp.h"
#include "launcher.h"

static long dispatch(seL4_UserContext *r) {
    static uint64_t random_state;
    static const uint64_t brk_start = 0x80000000UL;
    if (r->x0 < MAX_FDS && path_only(&fds[r->x0])) {
        switch (r->x8) {
        case 29: case 32: case 46: case 52: case 61: case 62: case 63: case 64:
        case 65: case 66: case 67: case 68: case 82: case 83: return -9;
        case 25:
            if (r->x1 != 0 && r->x1 != 1 && r->x1 != 2 && r->x1 != 3 && r->x1 != 1030) return -9;
        }
    }
    switch (r->x8) {
    case 444: return land_create(r->x0, r->x1, r->x2);
    case 445: return land_add(r->x0, r->x1, r->x2, r->x3);
    case 446: return land_restrict(r->x0, r->x1);
    case 19: return event_open(r->x0, r->x1);
    case 199: return local_socketpair(r);
    case 198:
        if (r->x0 == 1) return local_open(r->x1, r->x2);
#ifdef VIOLET_NETWORK
        return inet_open(r->x0, r->x1, r->x2);
#else
        return -97;
#endif
    case 200: case 201: case 202: case 203: case 204: case 205: case 206: case 207:
    case 208: case 209: case 210: case 211: case 212: case 242: case 269: {
        if (r->x0 >= MAX_FDS || (!fds[r->x0].file && !fds[r->x0].pipe)) return -9;
        struct local_socket *local = local_of(fds[r->x0].file);
        if (local) return local_call(local, r);
#ifdef VIOLET_NETWORK
        if (fds[r->x0].inet) return inet_call(r);
#endif
        return -88;
    }
    case 17: { // cwd is the root of the file bundle
        char cwd[] = "/";
        if (r->x1 < sizeof(cwd)) return -34;
        return transfer(r->x0, cwd, sizeof(cwd), true) ? 2 : -14;
    }
    case 174: case 175: case 176: case 177: return 1000; // real/effective uid and gid
    case 82: case 83: // synchronous in-memory writes
        return r->x0 < MAX_FDS && fds[r->x0].cursor ? 0 : -9;
    case 38: return rename_guest(r, 0);
    case 276: return rename_guest(r, r->x4);
    case 34: return mkdir_guest(r->x0, r->x1, r->x2);
    case 35: return unlink_guest(r->x0, r->x1, r->x2);
    case 61:
        if (r->x0 >= MAX_FDS || !fds[r->x0].file || !fds[r->x0].cursor) return -9;
        if (tmp_node_of(fds[r->x0].file)) return tmp_getdents(r->x0, r->x1, r->x2);
        if (fds[r->x0].file == &task_dir) return proc_getdents(&fds[r->x0], r->x1, r->x2, true);
        if (same(fds[r->x0].file->name, "/proc/self/fd")) return proc_getdents(&fds[r->x0], r->x1, r->x2, false);
        return packed_getdents(r->x0, r->x1, r->x2);
    case 279: return memfd_guest(r->x0, r->x1);
    case 52: {
        if (r->x0 >= MAX_FDS || !fds[r->x0].file) return -9;
        struct tmp_node *n = tmp_node_of(fds[r->x0].file);
        if (!n) return -30;
        n->mode = (n->mode & 0170000) | (r->x1 & 0777);
        return 0;
    }
    case 46: {
        if (r->x0 >= MAX_FDS || !fds[r->x0].cursor) return -9;
        if (!(fds[r->x0].cursor->flags & 3)) return -22;
        return tmp_resize(tmp_node_of(fds[r->x0].file), r->x1);
    }
    case 44: // fstatfs
        if (r->x0 >= MAX_FDS || !fds[r->x0].file) return -9;
        return guest_statfs(fds[r->x0].file, r->x1);
    case 43: case 48: case 79: {
        if (r->x8 == 79 && (r->x3 & 0x1000)) {
            char first = 0;
            if (r->x1 && !transfer(r->x1, &first, 1, false)) return -14;
            if (!first) return stat_fd(r->x0, r->x2);
        }
        char path[512];
        int err = r->x8 == 43 ? get_path_at(-100, r->x0, path, sizeof(path)) : get_path_at(r->x0, r->x1, path, sizeof(path));
        if (err) return err;
        struct tmp_node *n = tmp_lookup(path);
        const struct file *f = n ? &n->file : find_file(path);
        if (!f) f = char_device(path);
        if (!f) f = display_node(path);
        if (!f && same(path, ptmx_node.name)) f = &ptmx_node;
        if (!f && same(path, process_status.name)) f = &process_status;
        if (!f && same(path, proc_maps_entry.name)) f = maps_snapshot();
        if (!f && same(path, overcommit.name)) f = &overcommit;
        if (!f) f = same(path, task_dir.name) ? &task_dir : packed_dir(path);
        bool dir = f == &task_dir || is_packed_dir(f);
        if (!f && !dir) return -2;
        if (r->x8 == 43) return guest_statfs(f, r->x1);
        return r->x8 == 79 ? stat_to_guest(f, dir, r->x2) : ((r->x2 & 2) && !n ? -13 : 0);
    }
    case 62: {
        if (r->x0 >= MAX_FDS || !fds[r->x0].file) return -9;
        struct descriptor *d = &fds[r->x0];
        if (!d->cursor) return -29;
        if (r->x2 > 2) return -22;
        int64_t base = r->x2 == 2 ? (int64_t)d->file->size : r->x2 == 1 ? (int64_t)d->cursor->pos : 0;
        int64_t pos;
        if (__builtin_add_overflow(base, (int64_t)r->x1, &pos) || pos < 0) return -22;
        return d->cursor->pos = pos;
    }
    case 67: case 68: {
        if (r->x0 >= MAX_FDS || !fds[r->x0].file) return -9;
        struct descriptor *d = &fds[r->x0];
        if (!d->cursor) return -29;
        size_t old = d->cursor->pos;
        d->cursor->pos = r->x3;
        long count = stream_io(r->x0, r->x1, r->x2, r->x8 == 68);
        d->cursor->pos = old;
        return count;
    }
    case 78: return readlink_guest(r->x0, r->x1, r->x2, r->x3);
    case 80: return stat_fd(r->x0, r->x1);
    case 56: return open_path(r->x0, r->x1, r->x2, r->x3);
    case 57: return close_fd(r->x0);
    case 23: return duplicate_fd(r->x0, 0, false, false);
    case 24: return duplicate_fd(r->x0, r->x1, true, r->x2 & 0x80000);
    case 25: {
#ifdef VIOLET_TRACE
        say("fcntl cmd "); hex(r->x1); say(" val "); hex(r->x2); say("\n");
#endif
        if (r->x0 >= MAX_FDS || (!fds[r->x0].file && !fds[r->x0].pipe)) return -9;
        switch (r->x1) {
        case 1033: case 1034: return memfd_seal(&fds[r->x0], r->x1, r->x2);
        case 5: case 6: case 7: return file_lock(&fds[r->x0], r->x1, r->x2);
        case 0: case 1030: return duplicate_fd(r->x0, r->x2, false, r->x1 == 1030);
        case 3: case 4: {
            struct descriptor *d = &fds[r->x0];
            struct event_counter *counter = event_of(d->file);
            if (counter) {
                if (r->x1 == 3) return 2 | (counter->flags & 0x800);
                counter->flags = (counter->flags & 1) | (r->x2 & 0x800); return 0;
            }
            struct local_socket *local = local_of(d->file);
            if (local) {
                if (r->x1 == 3) return local->flags | 2;
                local->flags = r->x2 & 0x800; return 0;
            }
#ifdef VIOLET_NETWORK
            if (d->inet) return inet_flags(d->inet, r->x1, r->x2);
#endif
            if (d->pipe) {
                if (r->x1 == 3) return d->writer | (d->pipe->nonblock[d->writer] ? 0x800 : 0);
                d->pipe->nonblock[d->writer] = !!(r->x2 & 0x800);
                return 0;
            }
            if (!d->cursor) return -22;
            if (r->x1 == 3) return d->cursor->flags & ~0x80000U;
            unsigned mutable = 0x400 | 0x800;
            d->cursor->flags = (d->cursor->flags & ~mutable) | (r->x2 & mutable);
            return 0;
        }
        case 1: return fds[r->x0].cloexec;
        case 2: fds[r->x0].cloexec = r->x2 & 1; return 0;
        default: return -22;
        }
    }
    case 59: return make_pipe(r->x0, r->x1);
    case 63: case 64: return stream_io(r->x0, r->x1, r->x2, r->x8 == 64);
    case 65: case 66: {
        if (r->x2 > 1024) return -22;
        long total = 0;
        for (size_t i = 0; i < r->x2; i++) {
            uint64_t span[2];
            if (!transfer(r->x1 + i * 16, span, sizeof(span), false)) return total ? total : -14;
            long count = stream_io(r->x0, span[0], span[1], r->x8 == 66);
            if (total && caller->state == RETRY_IO) {
                // Earlier vectors were consumed; userspace retries the remainder.
                caller->state = RUNNING; caller->deadline = 0; caller->reply[8] += 4;
                return total;
            }
            if (count < 0) return total ? total : count;
            total += count;
            if ((size_t)count != span[1]) break;
        }
        return total;
    }
    case 29: {
        unsigned side;
        struct tty_pair *tty = r->x0 < MAX_FDS ? tty_of(fds[r->x0].file, &side) : NULL;
        if (tty) return tty_ioctl(tty, side, r->x1, r->x2);
        struct local_socket *local = r->x0 < MAX_FDS ? local_of(fds[r->x0].file) : NULL;
        if (local && r->x1 == 0x541b) {
            int bytes = local->tail - local->head;
            return transfer(r->x2, &bytes, sizeof(bytes), true) ? 0 : -14;
        }
        if (local && r->x1 == 0x5421) {
            int nonblock;
            if (!transfer(r->x2, &nonblock, 4, false)) return -14;
            local->flags = nonblock ? 0x800 : 0; return 0;
        }
        return screen_ioctl(r->x0, r->x1, r->x2);
    }
    case 261: { // prlimit64
        if (r->x0 && r->x0 != 1) return -3;
        uint64_t limit[2], updated[2];
        if (r->x1 == 3) { limit[0] = 8 << 20; limit[1] = ~0UL; }
        else if (r->x1 == 7) { limit[0] = fd_limit; limit[1] = fd_hard_limit; }
        else return -22;
        if (r->x2) {
            if (!transfer(r->x2, updated, sizeof(updated), false)) return -14;
            if (updated[0] > updated[1]) return -22;
            if (updated[1] > limit[1]) return -1;
            if (r->x1 != 7) return -1;
        }
        if (r->x3 && !transfer(r->x3, limit, sizeof(limit), true)) return -14;
        if (r->x2) { fd_limit = updated[0]; fd_hard_limit = updated[1]; }
        return 0;
    }
    case 179: { // sysinfo describes memory committed by this bootstrap runtime
        uint64_t info[14] = {0};
        info[4] = (uint64_t)FRAMES * PAGE; info[5] = (uint64_t)(FRAMES - live_pages) * PAGE;
        info[10] = 1; info[13] = 1;
        return transfer(r->x0, info, sizeof(info), true) ? 0 : -14;
    }
    case 73: { // Linux ppoll
        if (r->x1 > fd_limit) return -22;
        int ready = 0;
        uint64_t watched = 0;
        bool external = false;
        struct pollfd { int32_t fd; int16_t events, revents; } fd;
        for (unsigned i = 0; i < r->x1; i++) {
            uintptr_t addr = r->x0 + i * sizeof(fd);
            if (!transfer(addr, &fd, sizeof(fd), false)) return -14;
            unsigned side;
            struct tty_pair *tty = (unsigned)fd.fd < MAX_FDS ? tty_of(fds[fd.fd].file, &side) : NULL;
            if (tty) { fd.revents = tty_poll(tty, side, fd.events); external = true; }
            else
#ifdef VIOLET_NETWORK
            if ((unsigned)fd.fd < MAX_FDS && fds[fd.fd].inet) {
                fd.revents = inet_poll(fds[fd.fd].inet, fd.events); external = true;
            }
            else
#endif
            if (fd.fd < 0) fd.revents = 0;
            else if ((unsigned)fd.fd >= MAX_FDS) fd.revents = 32;
            else if (event_of(fds[fd.fd].file)) {
                watched |= poll_key(fds[fd.fd].file);
                uint64_t value = event_of(fds[fd.fd].file)->value;
                fd.revents = (value ? fd.events & 1 : 0) | (value < UINT64_MAX - 1 ? fd.events & 4 : 0);
            }
            else if (local_of(fds[fd.fd].file)) {
                watched |= poll_key(fds[fd.fd].file);
                fd.revents = local_poll(local_of(fds[fd.fd].file), fd.events);
            }
            else if (fds[fd.fd].pipe) {
                struct descriptor *d = &fds[fd.fd]; struct pipe *p = d->pipe;
                watched |= poll_key(p);
                int ev = d->writer ? (!p->readers ? 8 : p->count < sizeof(p->data) ? 4 : 0)
                                   : ((!p->writers ? 16 : 0) | (p->count ? 1 : 0));
                fd.revents = ev & (fd.events | 24);
            } else if (fds[fd.fd].file == &console) fd.revents = fds[fd.fd].writer ? (fd.events & 4) : 16;
            else fd.revents = fds[fd.fd].file ? (fd.events & 5) : 32;
            if (!transfer(addr, &fd, sizeof(fd), true)) return -14;
            if (fd.revents) ready++;
        }
        if (!ready) {
            if (!caller->poll_deadline) {
                uint64_t end = UINT64_MAX;
                long err = r->x2 ? time_deadline(r->x2, false, false, &end) : 0;
                if (err) return err;
                caller->poll_deadline = end;
            }
            uint64_t now = ticks_now();
            if (now < caller->poll_deadline) {
                caller->poll_watch = watched;
                uint64_t next = caller->poll_deadline;
                if (external && now + timer_hz / 200 < next) next = now + timer_hz / 200;
                return retry_io(next == UINT64_MAX ? 0 : next);
            }
        }
        caller->poll_deadline = 0;
        return ready;
    }
    case 123: { // sched_getaffinity, one virtual CPU
        if (r->x1 < 8) return -22;
        uint64_t mask = 1;
        return transfer(r->x2, &mask, sizeof(mask), true) ? 8 : -14;
    }
    case 167:
        switch (r->x0) {
        case 38: // No guest ELF or Linux syscall can grant privileges on exec.
            if (r->x1 != 1 || r->x2 || r->x3 || r->x4) return -22;
            caller->no_new_privs = true;
            return 0;
        case 39:
            if (r->x1 || r->x2 || r->x3 || r->x4) return -22;
            return caller->no_new_privs;
        case 1: // PR_SET_PDEATHSIG for helper processes
            if (r->x1 != 0 && r->x1 != 9) return -22;
            active_process->parent_signal = r->x1;
            return 0;
        case 2: return transfer(r->x1, &active_process->parent_signal, 4, true) ? 0 : -14;
        case 15: {
            char name[16] = {0};
            for (unsigned i = 0; i < 15; i++) {
                if (!transfer(r->x1 + i, name + i, 1, false)) return -14;
                if (!name[i]) break;
            }
            memcpy(caller->name, name, sizeof(name)); return 0;
        }
        case 16: return transfer(r->x1, caller->name, 16, true) ? 0 : -14;
        default: return -22;
        }
    case 436: { // each process owns its descriptor table, including after vfork
        unsigned first = r->x0, last = r->x1, flags = r->x2;
        if (first > last || (flags & ~6u)) return -22;
        for (unsigned i = first; i < MAX_FDS && i <= last; i++) if (fds[i].file || fds[i].pipe) {
            if (flags & 4) fds[i].cloexec = true;
            else close_fd(i);
        }
        return 0;
    }
    case 129: return kill_process((int)r->x0, r->x1);
    case 130: return kill_thread(0, (int)r->x0, r->x1);
    case 131: return (int)r->x0 > 0 ? kill_thread((int)r->x0, (int)r->x1, r->x2) : -22;
    case 139: read_full_context(r); return signal_return(r);
    case 277: return install_filter(r);
    case 154: return set_group((int)r->x0, (int)r->x1);
    case 155: case 156: {
        struct process *p = process_by_pid((int)r->x0);
        return p ? (r->x8 == 155 ? p->pgid : p->sid) : -3;
    }
    case 157: return new_session();
    case 172: return active_process->pid;
    case 173: return active_process->parent;
    case 178: return caller->tid;
    case 220:
        read_full_context(r);
        if (r->x0 & 0x10000) return clone_thread(r, caller->reply[8]);
        return clone_process(r, caller->reply[8]);
    case 221: return exec_guest(r);
    case 260: return wait_child(r);
    case 98: return futex_call(r);
    case 101: return sleep_until(r->x0, false, false); // nanosleep
    case 115: // clock_nanosleep
        if (r->x0 > 1 || r->x1 > 1) return -22;
        return sleep_until(r->x2, r->x1 == 1, r->x0 == 0);
    case 169: case 113: { // gettimeofday and clock_gettime use the same epoch
        bool timeval = r->x8 == 169;
        unsigned clock = timeval ? 0 : r->x0;
        if (clock != 0 && clock != 1 && clock != 4 && clock != 5 && clock != 6 && clock != 7) return -22;
        uint64_t ticks, hz;
        __asm__ volatile("mrs %0, cntpct_el0" : "=r"(ticks));
        __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(hz));
        ticks -= start_ticks;
        uint64_t ns = ticks / hz * 1000000000 + (ticks % hz) * 1000000000 / hz;
        if (clock == 0 || clock == 5) {
            if (!boot_entropy.epoch_ns) return -38;
            ns += boot_entropy.epoch_ns;
        }
        uint64_t ts[] = {ns / 1000000000, ns % 1000000000};
        if (!timeval) return transfer(r->x1, ts, sizeof(ts), true) ? 0 : -14;
        ts[1] /= 1000;
        uint64_t timezone = 0;
        if (r->x1 && !transfer(r->x1, &timezone, sizeof(timezone), true)) return -14;
        return (!r->x0 || transfer(r->x0, ts, sizeof(ts), true)) ? 0 : -14;
    }
    case 96:
        caller->clear_tid = r->x0;
        return caller->tid;
    case 132:
        if (r->x1 && !transfer(r->x1, caller->altstack, 24, true)) return -14;
        if (r->x0 && !transfer(r->x0, caller->altstack, 24, false)) return -14;
        return 0;
    case 134:
        if (!r->x0 || r->x0 >= 65 || r->x3 != 8 || (r->x1 && (r->x0 == 9 || r->x0 == 19))) return -22;
        if (r->x2 && !transfer(r->x2, active_process->actions[r->x0], 32, true)) return -14;
        if (r->x1 && !transfer(r->x1, active_process->actions[r->x0], 32, false)) return -14;
        return 0;
    case 135: {
        if (r->x3 != 8) return -22;
        uint64_t how = r->x0, set = 0;
        if (r->x1 && !transfer(r->x1, &set, 8, false)) return -14;
        if (r->x2 && !transfer(r->x2, &caller->sigmask, 8, true)) return -14;
        if (r->x1) {
            set &= ~((1ULL << 8) | (1ULL << 18));
            if (how == 0) caller->sigmask |= set;
            else if (how == 1) caller->sigmask &= ~set;
            else if (how == 2) caller->sigmask = set;
            else return -22;
        }
        return 0;
    }
    case 214: { // brk returns the previous break on failure
        uint64_t target = r->x0;
        if (target < brk_start || target >= next_map) return vm->brk;
        if (up(target) > up(vm->brk) && !add_region(up(vm->brk), up(target), 3)) return vm->brk;
        if (up(target) < up(vm->brk) && change(up(target), up(vm->brk) - up(target), 0, true)) return vm->brk;
        return vm->brk = target;
    }
    case 216: // musl probes the mapping below the stack with in-place mremap
        if (!region_at(r->x0)) return -14;
        return -12;
    case 278: // GRND_INSECURE permits a non-cryptographic source
        if (r->x2 & ~7UL) return -22;
        if (r->x0 > USER_END || r->x1 > USER_END - r->x0) return -14;
        if (entropy_available() || !r->x1) return read_entropy(r->x0, r->x1, r->x2 & 1);
        if (entropy_channel.online && !(r->x2 & 5)) return retry_io(ticks_now() + timer_hz/200);
        if (!(r->x2 & 4)) return -11;
        if (!random_state) { __asm__ volatile("mrs %0, cntpct_el0" : "=r"(random_state)); random_state |= 1; }
        for (size_t i = 0; i < r->x1; i++) {
            random_state ^= random_state << 13; random_state ^= random_state >> 7; random_state ^= random_state << 17;
            unsigned char byte = random_state;
            if (!transfer(r->x0 + i, &byte, 1, true)) return i ? (long)i : -14;
        }
        return r->x1;
    case 93: case 94:
        exit_thread(r->x8 == 94, r->x0);
        return 0;
    case 215: return change(r->x0, r->x1, 0, true); // munmap
    case 222: { // private anonymous or read-only-file-backed mmap
        const struct file *f = NULL;
        if (!(r->x3 & 0x20)) {
            if (r->x4 >= MAX_FDS || !(f = fds[r->x4].file)) return -9;
            if (path_only(&fds[r->x4])) return -9;
            if (fds[r->x4].cursor && (fds[r->x4].cursor->flags & 3) == 1) return -13;
            if (f == &screen_node) return display_mmap(r);
            if (r->x5 % PAGE) return -22;
        }
        unsigned kind = r->x3 & 3;
        if ((r->x3 & ~0x24833UL) || !r->x1 || (r->x2 & ~7UL)) return -22;
        if (kind != 2 && !(kind == 1 && f)) return -22;
        struct tmp_node *tmp = tmp_node_of(f);
        bool shared_ro = kind == 1 && (!tmp || !(fds[r->x4].cursor->flags & 3));
        if (kind == 1 && tmp && (r->x2 & 2) && (tmp->seals & 24)) return -1;
        if (shared_ro && (r->x2 & 2)) return -13; // the file image is immutable
        if (r->x1 >= USER_END) return -12;
        uint64_t len = up(r->x1), lo = 0, hi = 0;
        if (!(r->x3 & 0x10)) {
            if (r->x0 && !(r->x0 % PAGE) && gap_after(r->x0, len) == r->x0) lo = r->x0;
            if (!lo) lo = gap_after(next_map, len);
            if (!lo) lo = gap_after(0x200000000ULL, len);
            if (!lo) return -12;
            hi = lo + len;
        }
        if (r->x3 & 0x10) {
            lo = r->x0;
            if (!lo || lo % PAGE || lo >= USER_END || up(r->x1) > USER_END - lo) return -22;
            hi = lo + up(r->x1);
            long err = change(lo, hi - lo, 0, true);
            if (err) return err;
        }
        if (!add_region(lo, hi, r->x2)) return -12;
        regions[nregions - 1].shared_readonly = shared_ro;
        if (tmp) {
            struct region *v = &regions[nregions - 1];
            v->tmp = tmp; v->tmp_offset = r->x5; v->tmp_base = lo; v->tmp_shared = kind == 1;
        }
        if (!(r->x3 & 0x10) && lo != r->x0) next_map = hi;
        if (f && !tmp && r->x5 < f->size) {
            struct region *v = &regions[nregions - 1];
            size_t bytes = f->size - r->x5;
            if (bytes > hi - lo) bytes = hi - lo;
            v->data = f->data + r->x5; v->data_lo = lo; v->data_hi = lo + bytes;
        }
        return lo;
    }
    case 233: return advise_pages(r->x0, r->x1, r->x2);
    case 226: return change(r->x0, r->x1, r->x2, false);
    default: return -38;
    }
}

static void print_code_file(uintptr_t pc) {
    if (pc >= VDSO_BASE && pc < SIGNAL_GATE) { say(" ([vdso]+"); hex(pc - VDSO_BASE); say(")"); return; }
    struct region *code = region_at(pc);
    if (!code || !code->data || pc < code->data_lo || pc >= code->data_hi) return;
    uintptr_t byte = (uintptr_t)code->data + pc - code->data_lo;
    for (const struct file *f = fs_begin; f < fs_end; ++f) {
        uintptr_t base = (uintptr_t)f->data;
        if (byte >= base && byte - base < f->size) {
            say(" ("); say(f->name); say("+"); hex(byte - base); say(")");
            return;
        }
    }
}

static void print_fault_stack(void) {
    seL4_UserContext registers;
    read_full_context(&registers);
    say(" lr "); hex(registers.x30); print_code_file(registers.x30);
    uintptr_t fp = registers.x29;
    for (unsigned depth = 0; depth < 12; ++depth) {
        uintptr_t frame[2];
        if (fp < registers.sp || fp % 16 || !transfer(fp, frame, sizeof(frame), false)) break;
        if (!frame[1] || frame[1] >= USER_END) break;
        say(" frame "); hex(frame[1]); print_code_file(frame[1]);
        if (frame[0] <= fp) break;
        fp = frame[0];
    }
}

void root_main(seL4_BootInfo *info) {
    __asm__ volatile("mrs %0, cntpct_el0" : "=r"(start_ticks));
    bi = info; __sel4_ipc_buffer = bi->ipcBuffer; slot = bi->empty.start;
    for (unsigned i = 0; i < bi->untyped.end - bi->untyped.start; i++) {
        if (!bi->untypedList[i].isDevice && (!preferred_untyped ||
            bi->untypedList[i].sizeBits > bi->untypedList[preferred_untyped - bi->untyped.start].sizeBits))
            preferred_untyped = bi->untyped.start + i;
    }
    say("VioletOS AArch64 Open Runtime\n");
    processes[0].vm = acquire_space();
    processes[0].image = &fs_begin[0];
    processes[0].pid = processes[0].sid = processes[0].pgid = 1;
    activate(&processes[0]);
    tmp_make("/tmp", TMP_DIR | 01777, true);
    fd_limit = 1024; fd_hard_limit = MAX_FDS;
    for (unsigned i = 0; i < 3; i++)
        fds[i] = (struct descriptor){.file = &console, .writer = i != 0};
    seL4_UserContext ctx = {0};
    if (!load(&ctx, active_process->image, argv_start, argv_end - argv_start, 0)) { say("unsupported guest ELF\n"); halt(); }
    endpoint = alloc_object(seL4_EndpointObject, 0);
    response = alloc_object(seL4_ReplyObject, 0);
    guest_caps = alloc_object(seL4_CapTableObject, 4);
    setup_timer();
#ifdef VIOLET_NETWORK
    setup_network();
#endif
    start_thread(&threads[0], &ctx, 0);
    seL4_Word badge;
    seL4_MessageInfo_t tag = seL4_Recv(endpoint, &badge, response);
    for (;;) {
        if (badge == TIMER_BADGE) {
            timer_fired();
            tag = seL4_Recv(endpoint, &badge, response);
            continue;
        }
        if (!badge || badge > THREAD_LIMIT) { say("unknown fault badge\n"); halt(); }
        caller = &threads[badge - 1];
        activate(caller->owner);
        unsigned length = 0;
        switch (seL4_MessageInfo_get_label(tag)) {
        case seL4_Fault_VMFault: {
            unsigned long va = seL4_GetMR(seL4_VMFault_Addr);
            uintptr_t pc = seL4_GetMR(seL4_VMFault_IP);
            bool exec = seL4_GetMR(seL4_VMFault_PrefetchFault);
            bool write = (seL4_GetMR(seL4_VMFault_FSR) & 64) != 0;
            if (!guest_ptr(va, exec ? 4 : write ? 2 : 1)) {
                say("unhandled page fault "); hex(va); say(" in "); say(active_process->image->name);
                say(" tid "); hex(caller->tid); say(" pc "); hex(pc);
                struct region *fault_region = region_at(va);
                say(" frames "); hex(live_pages); say(" tmpfs "); hex(used_blocks);
                if (fault_region) {
                    say(" mapped prot "); hex(fault_region->prot);
                    if (fault_region->tmp) { say(" file "); say(fault_region->tmp->path); }
                } else say(" unmapped");
                print_code_file(pc);
                print_fault_stack();
                say("\n");
                active_process->death_signal = 11;
                exit_thread(true, 139);
                tag = seL4_Recv(endpoint, &badge, response);
                continue;
            }
            break;
        }
        case seL4_Fault_UnknownSyscall: {
            uint64_t pc = seL4_GetMR(seL4_UnknownSyscall_FaultIP);
            uint64_t args[9];
            for (unsigned i = 0; i < 8; ++i) args[i] = seL4_GetMR(i);
            args[8] = pc + 4;
            memcpy(caller->reply, args, sizeof(args));
            // seL4 AArch64 stores x0..x8 contiguously in its user context.
            _Static_assert(offsetof(seL4_UserContext, x8) - offsetof(seL4_UserContext, x0) == 8 * sizeof(long),
                           "AArch64 syscall argument layout");
            memcpy(&ctx.x0, args, 8 * sizeof(long));
            ctx.x8 = seL4_GetMR(seL4_UnknownSyscall_Length);
            long result;
            if (!caller->filters || !check_filter(&ctx, pc, &result)) result = dispatch(&ctx);
            if (caller->exec_done) {
                caller->exec_done = false;
                tag = seL4_Recv(endpoint, &badge, response);
                continue;
            }
            if (caller->state != RUNNING) {
                if (caller->state != UNUSED) {
                    seL4_CPtr empty = caller->saved_reply;
                    caller->saved_reply = response;
                    response = empty;
                    program_timer();
                }
                tag = seL4_Recv(endpoint, &badge, response);
                continue;
            }
#ifdef VIOLET_TRACE
            say("pid "); hex(active_process->pid); say(" sys "); hex(ctx.x8); say(" arg "); hex(ctx.x0); say(" => "); hex(result); say("\n");
#endif
            args[0] = result;
            length = sizeof(args) / sizeof(*args);
            for (unsigned i = 0; i < length; i++) seL4_SetMR(i, args[i]);
            break;
        }
        default: {
            uint64_t pc = seL4_GetMR(0);
            say("guest exception "); hex(seL4_MessageInfo_get_label(tag)); say(" pc "); hex(pc);
            say(" in "); say(active_process->image->name); say(" tid "); hex(caller->tid);
            print_code_file(pc);
            unsigned char *insn = guest_ptr(pc, 4);
            if (insn) { uint32_t op; memcpy(&op, insn, 4); say(" instruction "); hex(op); }
            print_fault_stack();
            say("\n");
            active_process->death_signal = seL4_MessageInfo_get_label(tag) == seL4_Fault_DebugException ? 5 : 4;
            exit_thread(true, 128 + active_process->death_signal);
            tag = seL4_Recv(endpoint, &badge, response);
            continue;
        }
        }
        tag = seL4_ReplyRecv(endpoint, seL4_MessageInfo_new(0, 0, 0, length), &badge, response);
    }
}
