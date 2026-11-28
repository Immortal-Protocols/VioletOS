#pragma once
#include "../include/tasks.h"
#include "../include/vm.h"
// Bootstrap allocator shared by the handset and QEMU supervisors.
static seL4_BootInfo *boot;
static seL4_CPtr ram, next_slot;
static seL4_Word cursor[CONFIG_MAX_NUM_BOOTINFO_UNTYPED_CAPS];
struct task {
    seL4_CPtr pd, caps, thread, ipc;
    unsigned sections[32], count;
    unsigned cap_bits; // 0 means the default CAP_BITS-sized CSpace.
};
static struct task bootstrap;
static struct task *mapping;
// The frames the last map_device call made: WINDOW_COUNT of them in
// consecutive slots from WINDOW_FIRST, each WINDOW_STEP bytes, mapped at
// consecutive addresses. On AArch32 every window the supervisor maps is a
// frame size, so it is always one frame, the last capability made.
#if VM_AARCH64
static seL4_CPtr window_first;
static unsigned window_count, window_bits;
#define WINDOW_FIRST window_first
#define WINDOW_COUNT window_count
#define WINDOW_STEP ((seL4_Word)1 << window_bits)
#else
#define WINDOW_FIRST (next_slot - 1)
#define WINDOW_COUNT 1u
#define WINDOW_STEP 0u
#endif
static void stop(void) { for (;;) seL4_Yield(); }
static void check(int error) { if(error) stop(); }
static seL4_CPtr allocate(seL4_CPtr source,unsigned type,unsigned bits) {
    if(next_slot>=boot->empty.end) stop();
    check(seL4_Untyped_Retype(source,type,bits,seL4_CapInitThreadCNode,0,0,next_slot,1));
    return next_slot++;
}
static seL4_CPtr create(seL4_CPtr source,unsigned type) { return allocate(source,type,0); }
#if VM_AARCH64
// vm.h: a mapping makes its own tables (vm_page_map), at every level, so
// there is nothing to prepare per section.
static seL4_CPtr vm_table(void) { return create(ram,seL4_ARM_PageTableObject); }
static inline void ensure_table(struct task *task,seL4_Word va) { (void)task; (void)va; }
// The largest frame size (an index into vm_frame_bits) no larger than 2^bits
// that both addresses are aligned to.
static unsigned frame_size(unsigned bits,seL4_Word addresses) {
    for(unsigned i=0;i<VM_FRAME_SIZES;i++)
        if(vm_frame_bits[i]<=bits && !(addresses&(((seL4_Word)1<<vm_frame_bits[i])-1))) return i;
    stop(); return 0;
}
// Skip device untyped memory up to `pa`: plain untypeds of the largest size
// the cursor's alignment and the gap allow (targets/qemu64/bootstrap.rs
// `filler`), so a gap of any size costs a few capabilities, never a frame per page.
static void skip_to(unsigned i,seL4_Word pa) {
    while(cursor[i]<pa) {
        seL4_Word gap=pa-cursor[i];
        unsigned b=63-__builtin_clzll(gap);
        if(cursor[i] && (unsigned)__builtin_ctzll(cursor[i])<b) b=__builtin_ctzll(cursor[i]);
        if(b>seL4_MaxUntypedBits) b=seL4_MaxUntypedBits;
        if(b<seL4_MinUntypedBits) stop();
        allocate(boot->untyped.start+i,seL4_UntypedObject,b);
        cursor[i]+=(seL4_Word)1<<b;
    }
}
// The device untyped holding [pa, pa + 2^bits) whose cursor has not passed pa.
static unsigned device_index(seL4_Word pa,unsigned bits) {
    seL4_Word size=(seL4_Word)1<<bits;
    for(unsigned i=0;i<boot->untyped.end-boot->untyped.start;i++) {
        seL4_UntypedDesc d=boot->untypedList[i];
        if(!d.isDevice || d.sizeBits<bits || pa<cursor[i] || pa<d.paddr ||
           pa-d.paddr>((seL4_Word)1<<d.sizeBits)-size) continue;
        return i;
    }
    stop(); return 0;
}
// A window of any 2^bits: the largest frames its alignment allows (a 1 MiB
// window is 256 pages, 16 MiB eight 2 MiB frames), made in one go so they
// sit in consecutive slots (window_first), then each mapped.
static void *map_device_rights(seL4_Word pa,seL4_Word va,unsigned bits,seL4_CapRights_t rights) {
    seL4_Word size=(seL4_Word)1<<bits;
    if((pa|va)&(size-1)) stop();
    unsigned i=device_index(pa,bits), k=frame_size(bits,pa|va), piece=vm_frame_bits[k];
    unsigned count=1u<<(bits-piece);
    skip_to(i,pa);
    if(count>boot->empty.end-next_slot) stop();
    seL4_CPtr first=next_slot;
    for(unsigned made=0;made<count;) {
        unsigned n=count-made>CONFIG_RETYPE_FAN_OUT_LIMIT ? CONFIG_RETYPE_FAN_OUT_LIMIT : count-made;
        check(seL4_Untyped_Retype(boot->untyped.start+i,vm_frame_types[k],0,seL4_CapInitThreadCNode,0,0,next_slot,n));
        next_slot+=n;
        made+=n;
    }
    cursor[i]+=size;
    for(unsigned j=0;j<count;j++) {
        seL4_Word offset=(seL4_Word)j<<piece;
        seL4_ARM_Page_GetAddress_t a=seL4_ARM_Page_GetAddress(first+j);
        if(a.error || a.paddr!=pa+offset ||
           seL4_ARM_Page_Map(first+j,mapping->pd,va+offset,rights,seL4_ARM_ExecuteNever)) stop();
    }
    window_first=first;
    window_count=count;
    window_bits=piece;
    return (void *)va;
}
seL4_CPtr device_untyped(uint32_t pa,unsigned bits) {
    if(pa&(((seL4_Word)1<<bits)-1)) stop();
    unsigned i=device_index(pa,bits);
    skip_to(i,pa);
    seL4_CPtr cap=allocate(boot->untyped.start+i,seL4_UntypedObject,bits);
    cursor[i]+=(seL4_Word)1<<bits;
    return cap;
}
#else
static void ensure_table(struct task *task,unsigned va) {
    unsigned section=va>>20;
    for(unsigned i=0;i<task->count;i++) if(task->sections[i]==section) return;
    if(task->count>=sizeof(task->sections)/sizeof(*task->sections)) stop();
    check(seL4_ARM_PageTable_Map(create(ram,seL4_ARM_PageTableObject),task->pd,va,0));
    task->sections[task->count++]=section;
}
static void *map_device_rights(uint32_t pa,uint32_t va,unsigned bits,seL4_CapRights_t rights) {
    static const unsigned widths[]={24,20,16,12};
    static const unsigned types[]={seL4_ARM_SuperSectionObject,seL4_ARM_SectionObject,seL4_ARM_LargePageObject,seL4_ARM_SmallPageObject};
    unsigned size=1u<<bits;
    if((pa|va)&(size-1)) stop();
    for(unsigned i=0;i<boot->untyped.end-boot->untyped.start;i++) {
        seL4_UntypedDesc d=boot->untypedList[i];
        if(!d.isDevice || d.sizeBits<bits || pa<cursor[i] || pa<d.paddr ||
           pa-d.paddr>(1u<<d.sizeBits)-size) continue;
        while(cursor[i]<pa) {
            unsigned j=0, gap=pa-cursor[i];
            while(j<3 && ((1u<<widths[j])>gap || (cursor[i]&((1u<<widths[j])-1)))) j++;
            create(boot->untyped.start+i,types[j]);
            cursor[i]+=1u<<widths[j];
        }
        if(bits<20) ensure_table(mapping,va);
        seL4_CPtr cap=create(boot->untyped.start+i,bits==24?types[0]:bits==20?types[1]:bits==16?types[2]:types[3]);
        cursor[i]+=size;
        seL4_ARM_Page_GetAddress_t a=seL4_ARM_Page_GetAddress(cap);
        if(a.error || a.paddr!=pa || seL4_ARM_Page_Map(cap,mapping->pd,va,rights,seL4_ARM_ExecuteNever)) stop();
        return (void *)va;
    }
    stop(); return 0;
}
#endif

void *map_device(uint32_t pa,uint32_t va,unsigned bits) {
    return map_device_rights(pa,va,bits,seL4_AllRights);
}

#if !VM_AARCH64
// A device untyped covering [pa, pa + 2^bits), for delegating physical memory
// (for example the open-app pool) to another process. Same ascending-order
// rule as map_device: the cursor skips any gap with filler frames.
seL4_CPtr device_untyped(uint32_t pa,unsigned bits) {
    static const unsigned widths[]={24,20,16,12};
    static const unsigned types[]={seL4_ARM_SuperSectionObject,seL4_ARM_SectionObject,seL4_ARM_LargePageObject,seL4_ARM_SmallPageObject};
    unsigned size=1u<<bits;
    if(pa&(size-1)) stop();
    for(unsigned i=0;i<boot->untyped.end-boot->untyped.start;i++) {
        seL4_UntypedDesc d=boot->untypedList[i];
        if(!d.isDevice || d.sizeBits<bits || pa<cursor[i] || pa<d.paddr ||
           pa-d.paddr>(1u<<d.sizeBits)-size) continue;
        while(cursor[i]<pa) {
            unsigned j=0, gap=pa-cursor[i];
            while(j<3 && ((1u<<widths[j])>gap || (cursor[i]&((1u<<widths[j])-1)))) j++;
            create(boot->untyped.start+i,types[j]);
            cursor[i]+=1u<<widths[j];
        }
        seL4_CPtr cap=allocate(boot->untyped.start+i,seL4_UntypedObject,bits);
        cursor[i]+=size;
        return cap;
    }
    stop(); return 0;
}
#endif
