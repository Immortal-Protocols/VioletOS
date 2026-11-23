#pragma once
#include "../include/tasks.h"
// Bootstrap allocator shared by the handset and QEMU supervisors.
static seL4_BootInfo *boot;
static seL4_CPtr ram, next_slot;
static unsigned cursor[CONFIG_MAX_NUM_BOOTINFO_UNTYPED_CAPS];
struct task {
    seL4_CPtr pd, caps, thread, ipc;
    unsigned sections[32], count;
    unsigned cap_bits; // 0 means the default CAP_BITS-sized CSpace.
};
static struct task bootstrap;
static struct task *mapping;
static void stop(void) { for (;;) seL4_Yield(); }
static void check(int error) { if(error) stop(); }
static seL4_CPtr allocate(seL4_CPtr source,unsigned type,unsigned bits) {
    if(next_slot>=boot->empty.end) stop();
    check(seL4_Untyped_Retype(source,type,bits,seL4_CapInitThreadCNode,0,0,next_slot,1));
    return next_slot++;
}
static seL4_CPtr create(seL4_CPtr source,unsigned type) { return allocate(source,type,0); }
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

void *map_device(uint32_t pa,uint32_t va,unsigned bits) {
    return map_device_rights(pa,va,bits,seL4_AllRights);
}

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
