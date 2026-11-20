#include "tasks.h"
seL4_IPCBuffer *__sel4_ipc_buffer;
char __sel4_print_error;
static seL4_BootInfo *boot;
static seL4_CPtr ram, next_slot;
static unsigned cursor[CONFIG_MAX_NUM_BOOTINFO_UNTYPED_CAPS];
struct task {
    seL4_CPtr pd, caps, thread, ipc;
    unsigned sections[12], count;
};
static struct task bootstrap, ui, touch;
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
        ensure_table(mapping,va);
        seL4_CPtr cap=create(boot->untyped.start+i,bits==16?types[2]:types[3]);
        cursor[i]+=size;
        seL4_ARM_Page_GetAddress_t a=seL4_ARM_Page_GetAddress(cap);
        if(a.error || a.paddr!=pa || seL4_ARM_Page_Map(cap,mapping->pd,va,seL4_AllRights,seL4_ARM_ExecuteNever)) stop();
        return (void *)va;
    }
    stop(); return 0;
}
enum { SCRATCH=0x03000000, CAP_BITS=4, PAGE=4096, BASE=0x100000 };
extern char writable_start[], image_end[];
static void new_page(struct task *t,unsigned va,bool copy) {
    seL4_CPtr frame=create(ram,seL4_ARM_SmallPageObject);
    if(copy) {
        check(seL4_ARM_Page_Map(frame,bootstrap.pd,SCRATCH,seL4_AllRights,seL4_ARM_Default_VMAttributes));
        for(unsigned i=0;i<PAGE/4;i++) ((unsigned *)SCRATCH)[i]=((unsigned *)va)[i];
        check(seL4_ARM_Page_Unmap(frame));
    }
    ensure_table(t,va);
    check(seL4_ARM_Page_Map(frame,t->pd,va,seL4_AllRights,seL4_ARM_Default_VMAttributes|seL4_ARM_ExecuteNever));
    if(va==IPC_BUFFER) t->ipc=frame;
}
static void child_entry(void (*entry)(void)) {
    seL4_SetIPCBuffer((void *)IPC_BUFFER);
    entry();
    stop();
}
static void new_task(struct task *t,void (*entry)(void)) {
    t->pd=create(ram,seL4_ARM_PageDirectoryObject);
    check(seL4_ARM_ASIDPool_Assign(seL4_CapInitThreadASIDPool,t->pd));
    t->caps=allocate(ram,seL4_CapTableObject,CAP_BITS);
    t->thread=create(ram,seL4_TCBObject);
    ensure_table(t,BASE);
    for(unsigned va=BASE;va<(unsigned)writable_start;va+=PAGE) {
        if(next_slot>=boot->empty.end) stop();
        seL4_CPtr frame=boot->userImageFrames.start+(va-BASE)/PAGE;
        check(seL4_CNode_Copy(seL4_CapInitThreadCNode,next_slot,32,seL4_CapInitThreadCNode,frame,32,seL4_AllRights));
        check(seL4_ARM_Page_Map(next_slot++,t->pd,va,seL4_CapRights_new(0,0,1,0),seL4_ARM_Default_VMAttributes));
    }
    for(unsigned va=(unsigned)writable_start;va<(unsigned)image_end;va+=PAGE) new_page(t,va,true);
    new_page(t,STACK_TOP-2*PAGE,false);
    new_page(t,STACK_TOP-PAGE,false);
    new_page(t,IPC_BUFFER,false);
    unsigned guard=seL4_CNode_CapData_new(0,32-CAP_BITS).words[0];
    check(seL4_TCB_Configure(t->thread,0,t->caps,guard,t->pd,0,IPC_BUFFER,t->ipc));
    check(seL4_TCB_SetPriority(t->thread,seL4_CapInitThreadTCB,100));
    seL4_UserContext regs={.pc=(unsigned)child_entry,.sp=STACK_TOP,.r0=(unsigned)entry};
    check(seL4_TCB_WriteRegisters(t->thread,0,0,sizeof(regs)/sizeof(unsigned),&regs));
}
static void endpoint(struct task *t,unsigned slot,seL4_CPtr ep,bool server) {
    // GrantReply allows the caller to receive a reply without transferring caps.
    seL4_CapRights_t rights=seL4_CapRights_new(!server,0,server,!server);
    check(seL4_CNode_Copy(t->caps,slot,CAP_BITS,seL4_CapInitThreadCNode,ep,32,rights));
}
#ifdef VIOLET_TEST_ISOLATION
// Run one load with the UI's actual page directory and capability table.
static void read_probe(volatile unsigned *address) {
    (void)*address;
    stop();
}
static void check_isolation(volatile unsigned *report) {
    seL4_CPtr faults=create(ram,seL4_EndpointObject);
    endpoint(&ui,3,faults,false);
    unsigned guard=seL4_CNode_CapData_new(0,32-CAP_BITS).words[0];
    check(seL4_TCB_SetSpace(ui.thread,3,ui.caps,guard,ui.pd,0));
    unsigned address=report[0];
    seL4_UserContext regs={.pc=(unsigned)read_probe,.sp=STACK_TOP,.r0=address};
    check(seL4_TCB_WriteRegisters(ui.thread,1,0,sizeof(regs)/sizeof(unsigned),&regs));
    seL4_MessageInfo_t fault=seL4_Recv(faults,0);
    if(seL4_MessageInfo_get_label(fault)==seL4_Fault_VMFault && seL4_GetMR(1)==address)
        report[1]=address;
    stop();
}
#endif
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
    bootstrap.pd=seL4_CapInitThreadVSpace;
    ensure_table(&bootstrap,SCRATCH);
    new_task(&ui,stopwatch);
    new_task(&touch,touch_task);
    seL4_CPtr ep=create(ram,seL4_EndpointObject);
    endpoint(&touch,TOUCH_EP,ep,true);
    endpoint(&ui,TOUCH_EP,ep,false);
    mapping=&touch;
    touch_map();
#ifdef VIOLET_QEMU
    static struct task storage;
    new_task(&storage,storage_task);
    seL4_CPtr disk=create(ram,seL4_EndpointObject);
    endpoint(&storage,STORAGE_EP,disk,true);
    endpoint(&ui,STORAGE_EP,disk,false);
    mapping=&storage;
    map_device(0x0a000000,0x0a000000,12);
    map_device(0x49200000,0x0b000000,12);
    check(seL4_TCB_Resume(storage.thread));
#endif
#ifdef VIOLET_TEST_ISOLATION
    mapping=&bootstrap;
    volatile unsigned *report=map_device(0x49300000,0x09100000,12);
#endif
    mapping=&ui;
    for(unsigned offset=0;offset<720*1600*4;offset+=65536)
        map_device(0x79450000+offset,0x08000000+offset,16);
#ifdef VIOLET_TEST_ISOLATION
    check_isolation(report);
#endif
    check(seL4_TCB_Resume(touch.thread));
    check(seL4_TCB_Resume(ui.thread));
    seL4_TCB_Suspend(seL4_CapInitThreadTCB);
}
