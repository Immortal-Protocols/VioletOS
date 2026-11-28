// A stand-in for the AArch64 libsel4, for system/supervisor/tests/memory_test.c
// only: the types and calls platform/sel4/board/memory.h and vm.h use, with
// AArch64's sizes. The calls are the test's model of the kernel.
#pragma once
#include <stdbool.h>
#include <stdint.h>
typedef unsigned long long seL4_Word;
typedef seL4_Word seL4_CPtr;
typedef uint8_t seL4_Uint8;
typedef enum {
  seL4_NoError, seL4_InvalidArgument, seL4_InvalidCapability, seL4_IllegalOperation, seL4_RangeError,
  seL4_AlignmentError, seL4_FailedLookup, seL4_TruncatedMessage, seL4_DeleteFirst, seL4_RevokeFirst,
  seL4_NotEnoughMemory
} seL4_Error;
#define seL4_WordBits 64
#define seL4_PageBits 12
#define seL4_LargePageBits 21
#define seL4_HugePageBits 30
#define seL4_MinUntypedBits 4
#define seL4_MaxUntypedBits 47
enum { seL4_CapInitThreadCNode = 2 };
typedef struct { seL4_Word words[1]; } seL4_CapRights_t;
#define seL4_AllRights ((seL4_CapRights_t){{15}})
#define seL4_CanRead ((seL4_CapRights_t){{2}})
typedef enum {
  seL4_ARM_PageCacheable = 1, seL4_ARM_ParityEnabled = 2, seL4_ARM_Default_VMAttributes = 3, seL4_ARM_ExecuteNever = 4
} seL4_ARM_VMAttributes;
enum {
  seL4_UntypedObject, seL4_TCBObject, seL4_EndpointObject, seL4_NotificationObject, seL4_CapTableObject,
  seL4_ARM_SmallPageObject, seL4_ARM_LargePageObject, seL4_ARM_PageTableObject, seL4_ARM_HugePageObject,
  seL4_ARM_VSpaceObject
};
typedef struct { seL4_Word paddr; seL4_Uint8 sizeBits, isDevice, padding[6]; } seL4_UntypedDesc;
typedef struct { seL4_Word start, end; } seL4_SlotRegion;
typedef struct { seL4_SlotRegion empty, untyped; seL4_UntypedDesc untypedList[CONFIG_MAX_NUM_BOOTINFO_UNTYPED_CAPS]; } seL4_BootInfo;
typedef struct { int error; seL4_Word paddr; } seL4_ARM_Page_GetAddress_t;
seL4_Error seL4_Untyped_Retype(seL4_CPtr untyped, seL4_Word type, seL4_Word bits, seL4_CPtr root, seL4_Word index,
                               seL4_Word depth, seL4_Word offset, seL4_Word count);
seL4_Error seL4_ARM_Page_Map(seL4_CPtr frame, seL4_CPtr vspace, seL4_Word va, seL4_CapRights_t rights,
                             seL4_ARM_VMAttributes attributes);
seL4_Error seL4_ARM_PageTable_Map(seL4_CPtr table, seL4_CPtr vspace, seL4_Word va, seL4_ARM_VMAttributes attributes);
seL4_ARM_Page_GetAddress_t seL4_ARM_Page_GetAddress(seL4_CPtr frame);
void seL4_Yield(void);
