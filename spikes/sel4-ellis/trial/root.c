// The initial user task runs with its own address space and boot capabilities.
#include <sel4/config.h>
#undef CONFIG_LIB_SEL4_USE_THREAD_LOCALS
#include <sel4/sel4.h>
seL4_IPCBuffer *__sel4_ipc_buffer;
char __sel4_print_error;
enum { fb = 0x79450000, display_va = 0x08000000 };
static void stop(void) { for (;;) seL4_Yield(); }

static void create(seL4_CPtr untyped, unsigned type, seL4_CPtr slot)
{
    if (seL4_Untyped_Retype(untyped, type, 0, seL4_CapInitThreadCNode,
                            0, 0, slot, 1) != seL4_NoError) stop();
}

void root_main(seL4_BootInfo *bi)
{
    seL4_SetIPCBuffer(bi->ipcBuffer);
    seL4_CPtr ram = 0, device = 0, slot = bi->empty.start;
    unsigned cursor = 0;
    for (unsigned cap = bi->untyped.start; cap < bi->untyped.end; ++cap) {
        seL4_UntypedDesc d = bi->untypedList[cap - bi->untyped.start];
        if (!d.isDevice && d.sizeBits >= 12) ram = cap;
        if (d.isDevice && d.paddr <= fb &&
            d.sizeBits >= 16 && d.sizeBits < 32 && fb - d.paddr <= (1u << d.sizeBits) - 65536) {
            device = cap;
            cursor = d.paddr;
        }
    }
    if (!device || !ram || slot >= bi->empty.end) stop();
    create(ram, seL4_ARM_PageTableObject, slot);
    if (seL4_ARM_PageTable_Map(slot++, seL4_CapInitThreadVSpace, display_va, 0)) stop();
    while (cursor < fb) {
        unsigned bits = 24, type = seL4_ARM_SuperSectionObject;
        if (fb - cursor < (1u << bits) || (cursor & ((1u << bits)-1))) {
            bits = 20; type = seL4_ARM_SectionObject;
        }
        if (fb - cursor < (1u << bits) || (cursor & ((1u << bits)-1))) {
            bits = 16; type = seL4_ARM_LargePageObject;
        }
        if (fb - cursor < (1u << bits) || (cursor & ((1u << bits)-1))) {
            bits = 12; type = seL4_ARM_SmallPageObject;
        }
        if (slot >= bi->empty.end) stop();
        create(device, type, slot++);
        cursor += 1u << bits;
    }
    if (slot >= bi->empty.end) stop();
    create(device, seL4_ARM_LargePageObject, slot);
    seL4_ARM_Page_GetAddress_t address = seL4_ARM_Page_GetAddress(slot);
    if (address.error || address.paddr != fb) stop();
    if (seL4_ARM_Page_Map(slot, seL4_CapInitThreadVSpace, display_va,
                          seL4_AllRights, seL4_ARM_ExecuteNever)) stop();
    volatile unsigned *pixels = (volatile unsigned *)display_va;
    for (unsigned y = 0; y < 22; ++y)
        for (unsigned x = 0; x < 720; ++x)
            pixels[y * 736 + x] = 0xff00ff00;
    __asm__ volatile("dsb sy" ::: "memory");
    stop();
}
