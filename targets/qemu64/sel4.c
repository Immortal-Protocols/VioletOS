// The seL4 calls the 64-bit bootstrap task makes, as plain C functions for
// Rust (root.rs). Compiled against the AArch64 kernel's own libsel4 headers,
// so object types, invocation labels and the "svc #1" native call come from
// the kernel build, not from numbers typed here.
#include <sel4/config.h>
#undef CONFIG_LIB_SEL4_USE_THREAD_LOCALS
#include <sel4/sel4.h>

seL4_IPCBuffer *__sel4_ipc_buffer;
char __sel4_print_error;
static seL4_BootInfo *boot;
typedef unsigned long word;

void k_init(seL4_BootInfo *info) {
  boot = info;
  __sel4_ipc_buffer = info->ipcBuffer;
}
word k_empty_start(void) { return boot->empty.start; }
word k_empty_end(void) { return boot->empty.end; }
word k_untyped_count(void) { return boot->untyped.end - boot->untyped.start; }
// Untyped `index`: its capability; physical address, size bits and device flag through the pointers.
word k_untyped(word index, word *paddr, word *bits, word *device) {
  seL4_UntypedDesc d = boot->untypedList[index];
  *paddr = d.paddr;
  *bits = d.sizeBits;
  *device = d.isDevice;
  return boot->untyped.start + index;
}
enum { K_UNTYPED, K_TCB, K_ENDPOINT, K_CNODE, K_VSPACE, K_TABLE, K_FRAME_4K, K_FRAME_2M, K_FRAME_1G };
long k_retype(word untyped, word kind, word bits, word slot) {
  static const word types[] = {seL4_UntypedObject, seL4_TCBObject, seL4_EndpointObject, seL4_CapTableObject,
                               seL4_ARM_VSpaceObject, seL4_ARM_PageTableObject, seL4_ARM_SmallPageObject,
                               seL4_ARM_LargePageObject, seL4_ARM_HugePageObject};
  if (kind >= sizeof types / sizeof *types) return -1;
  return seL4_Untyped_Retype(untyped, types[kind], bits, seL4_CapInitThreadCNode, 0, 0, slot, 1);
}
// 0: mapped. 1: a page table is missing. 2: something is mapped there. Otherwise 0x100 + the kernel's error.
static long map_result(long error) {
  return error == seL4_NoError ? 0 : error == seL4_FailedLookup ? 1 : error == seL4_DeleteFirst ? 2 : 0x100 + error;
}
// protection: 0 read-only, 1 read-write, 2 read-execute. Never writable and executable.
long k_page_map(word frame, word vspace, word va, word protection, word device) {
  seL4_CapRights_t rights = protection == 1 ? seL4_ReadWrite : seL4_CanRead;
  word attributes = device ? 0 : seL4_ARM_Default_VMAttributes;
  if (protection != 2) attributes |= seL4_ARM_ExecuteNever;
  return map_result(seL4_ARM_Page_Map(frame, vspace, va, rights, (seL4_ARM_VMAttributes)attributes));
}
long k_page_unmap(word frame) { return seL4_ARM_Page_Unmap(frame); }
// Clean the data cache to the point of unification and invalidate the
// instruction cache for the first `bytes` of a mapped frame: code written
// through another mapping (the alias) is then what the program fetches.
long k_page_unify(word frame, word bytes) { return seL4_ARM_Page_Unify_Instruction(frame, 0, bytes); }
// Delete every capability derived from `slot` (an untyped: every object made
// from it, which unmaps frames and tables, suspends threads and frees ASIDs).
// The next retype from it starts again at its beginning.
long k_revoke(word slot) { return seL4_CNode_Revoke(seL4_CapInitThreadCNode, slot, seL4_WordBits); }
long k_table_map(word table, word vspace, word va) {
  return map_result(seL4_ARM_PageTable_Map(table, vspace, va, seL4_ARM_Default_VMAttributes));
}
long k_asid_assign(word vspace) { return seL4_ARM_ASIDPool_Assign(seL4_CapInitThreadASIDPool, vspace); }
long k_copy(word dest, word src) {
  return seL4_CNode_Copy(seL4_CapInitThreadCNode, dest, seL4_WordBits, seL4_CapInitThreadCNode, src, seL4_WordBits, seL4_AllRights);
}
long k_delete(word slot) { return seL4_CNode_Delete(seL4_CapInitThreadCNode, slot, seL4_WordBits); }
// Put a badged copy of `endpoint` in slot `index` of the CNode `cnode` (2^bits slots). The copy may
// send, and lets the receiver reply (the kernel delivers a thread's faults only through such a cap);
// it cannot receive, and cannot pass capabilities.
long k_mint_into(word cnode, word bits, word index, word endpoint, word badge) {
  return seL4_CNode_Mint(cnode, index, bits, seL4_CapInitThreadCNode, endpoint, seL4_WordBits,
                         seL4_CapRights_new(1, 0, 0, 1), badge);
}
// A thread whose whole capability space is `cnode` (2^bits slots) and whose faults go to slot `fault` of it.
long k_tcb_configure(word tcb, word cnode, word bits, word fault, word vspace) {
  word guard = seL4_CNode_CapData_new(0, seL4_WordBits - bits).words[0];
  long error = seL4_TCB_Configure(tcb, fault, cnode, guard, vspace, 0, 0, seL4_CapNull);
  return error ? error : seL4_TCB_SetPriority(tcb, seL4_CapInitThreadTCB, 100);
}
long k_tcb_write(word tcb, const word *context, word words) {
  return seL4_TCB_WriteRegisters(tcb, 0, 0, words, (seL4_UserContext *)context);
}
long k_tcb_read(word tcb, word *context, word words) {
  return seL4_TCB_ReadRegisters(tcb, 0, 0, words, (seL4_UserContext *)context);
}
long k_tcb_resume(word tcb) { return seL4_TCB_Resume(tcb); }
long k_tcb_suspend(word tcb) { return seL4_TCB_Suspend(tcb); }
// Wait on `endpoint`: the fault label, with the badge and up to `words` message words stored.
word k_receive(word endpoint, word *badge, word *mrs, word words) {
  seL4_MessageInfo_t tag = seL4_Recv(endpoint, badge);
  word n = seL4_MessageInfo_get_length(tag);
  for (word i = 0; i < words; i++) mrs[i] = i < n ? seL4_GetMR(i) : 0;
  return seL4_MessageInfo_get_label(tag);
}
// Answer the fault just received: the kernel copies `words` registers back and resumes the thread.
void k_reply(const word *mrs, word words) {
  for (word i = 0; i < words; i++) seL4_SetMR(i, mrs[i]);
  seL4_Reply(seL4_MessageInfo_new(0, 0, 0, words));
}
word k_vspace(void) { return seL4_CapInitThreadVSpace; }
void k_yield(void) { seL4_Yield(); }
void *memcpy(void *v, const void *s, unsigned long n) { unsigned char *d = v; const unsigned char *p = s; while (n--) *d++ = *p++; return v; }
void *memmove(void *v, const void *s, unsigned long n) {
  unsigned char *d = v; const unsigned char *p = s;
  if (d < p) while (n--) *d++ = *p++; else while (n--) d[n] = p[n];
  return v;
}
void *memset(void *v, int c, unsigned long n) { unsigned char *d = v; while (n--) *d++ = c; return v; }
int memcmp(const void *v, const void *s, unsigned long n) { const unsigned char *a = v, *b = s; while (n--) { if (*a != *b) return *a - *b; ++a; ++b; } return 0; }
int bcmp(const void *v, const void *s, unsigned long n) { return memcmp(v, s, n); }
