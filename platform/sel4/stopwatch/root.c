#include <sel4/config.h>
#undef CONFIG_LIB_SEL4_USE_THREAD_LOCALS
#include <sel4/sel4.h>
#include "platform.h"
seL4_IPCBuffer *__sel4_ipc_buffer;
char __sel4_print_error;
static seL4_BootInfo *boot;
static seL4_CPtr ram, next_slot;
static unsigned cursor[CONFIG_MAX_NUM_BOOTINFO_UNTYPED_CAPS];
static uint32_t tables[8];
static unsigned table_count;
static void stop(void) { for (;;) seL4_Yield(); }
static seL4_CPtr create(seL4_CPtr source, unsigned type) {
    if (next_slot >= boot->empty.end) stop();
    if (seL4_Untyped_Retype(source,type,0,seL4_CapInitThreadCNode,0,0,next_slot,1)) stop();
    return next_slot++;
}
static void ensure_table(uint32_t va) {
    uint32_t section=va>>20;
    for(unsigned i=0;i<table_count;i++) if(tables[i]==section) return;
    if(table_count>=sizeof(tables)/sizeof(*tables)) stop();
    if(seL4_ARM_PageTable_Map(create(ram,seL4_ARM_PageTableObject),seL4_CapInitThreadVSpace,va,0)) stop();
    tables[table_count++]=section;
}
void *map_device(uint32_t pa,uint32_t va,unsigned bits) {
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
        ensure_table(va);
        seL4_CPtr cap=create(boot->untyped.start+i,bits==16?types[2]:types[3]);
        cursor[i]+=size;
        seL4_ARM_Page_GetAddress_t a=seL4_ARM_Page_GetAddress(cap);
        if(a.error || a.paddr!=pa || seL4_ARM_Page_Map(cap,seL4_CapInitThreadVSpace,va,seL4_AllRights,seL4_ARM_ExecuteNever)) stop();
        return (void *)va;
    }
    stop(); return 0;
}
void root_main(seL4_BootInfo *bi) {
    boot=bi;
    seL4_SetIPCBuffer(bi->ipcBuffer);
    next_slot=bi->empty.start;
    unsigned largest=0;
    for(unsigned i=0;i<bi->untyped.end-bi->untyped.start;i++) {
        seL4_UntypedDesc d=bi->untypedList[i];
        cursor[i]=d.paddr;
        if(!d.isDevice && d.sizeBits>largest) { largest=d.sizeBits; ram=bi->untyped.start+i; }
    }
    stopwatch();
}
