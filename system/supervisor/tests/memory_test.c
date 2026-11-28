// Host test of the supervisor's AArch64 allocator (platform/sel4/board/
// memory.h with platform/sel4/include/vm.h): make test-unit. memory.h is
// compiled unchanged, on any host, against a model of the seL4 calls it makes
// (tests/model/sel4/sel4.h): untyped memory with the kernel's watermark rule,
// and an AArch64 address space of four levels whose mappings fail with
// FailedLookup while a table is missing, as seL4's do.
//
// What it checks: windows of every size the supervisor maps are cut into the
// frames AArch64 has (1 MiB into 4 KiB pages, 16 MiB into 2 MiB frames, 64
// KiB into pages), in consecutive slots, each at its physical address; the
// tables a mapping makes (three for a page in an empty space, none under a
// 2 MiB frame, none where the space already has them); device memory skipped
// by plain untypeds, above 4 GiB and with untypeds of 2^32 bytes and more;
// and that out-of-order and misaligned requests stop instead of mapping.
#define VIOLET_HOST_MODEL_AARCH64 1
#include <setjmp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../../../platform/sel4/board/memory.h"

static int failures;
#define EXPECT(what)                                                \
  do {                                                              \
    if (!(what)) {                                                  \
      printf("FAILED %s:%d: %s\n", __FILE__, __LINE__, #what);      \
      failures++;                                                   \
    }                                                               \
  } while (0)

// ---- The model kernel ------------------------------------------------------
enum { SLOTS = 1 << 14, LEAVES = 4096, TABLES = 256, FIRST_EMPTY = 64 };
enum kind { EMPTY, UNTYPED, FRAME, TABLE, VSPACE };
static struct cap {
  enum kind kind;
  seL4_Word paddr, watermark;
  unsigned bits;
  bool device, mapped;
} caps[SLOTS];
static struct space {
  seL4_Word table[TABLES][2]; // (span, va >> span)
  unsigned tables;
  seL4_Word leaf[LEAVES][3];  // (va, bits, attributes)
  unsigned leaves;
} spaces[4];
static const unsigned spans[3] = {39, 30, 21};
static unsigned tables_made;

static struct space *space_of(seL4_CPtr vspace) {
  return vspace < SLOTS && caps[vspace].kind == VSPACE ? &spaces[caps[vspace].paddr] : 0;
}
static bool has_table(struct space *s, unsigned span, seL4_Word va) {
  for (unsigned i = 0; i < s->tables; i++)
    if (s->table[i][0] == span && s->table[i][1] == va >> span) return true;
  return false;
}
// A leaf overlapping [va, va + 2^bits).
static bool overlaps(struct space *s, seL4_Word va, unsigned bits) {
  for (unsigned i = 0; i < s->leaves; i++) {
    seL4_Word start = s->leaf[i][0], end = start + ((seL4_Word)1 << s->leaf[i][1]);
    if (start < va + ((seL4_Word)1 << bits) && va < end) return true;
  }
  return false;
}
static unsigned object_bits(seL4_Word type, seL4_Word bits) {
  switch (type) {
  case seL4_UntypedObject: return (unsigned)bits;
  case seL4_ARM_SmallPageObject: return 12;
  case seL4_ARM_LargePageObject: return 21;
  case seL4_ARM_HugePageObject: return 30;
  case seL4_ARM_PageTableObject: return 12;
  default: return 0;
  }
}
seL4_Error seL4_Untyped_Retype(seL4_CPtr untyped, seL4_Word type, seL4_Word bits, seL4_CPtr root, seL4_Word index,
                               seL4_Word depth, seL4_Word offset, seL4_Word count) {
  struct cap *u = &caps[untyped];
  unsigned size = object_bits(type, bits);
  if (u->kind != UNTYPED || root != seL4_CapInitThreadCNode || index || depth || !size || !count ||
      count > CONFIG_RETYPE_FAN_OUT_LIMIT || offset + count > SLOTS)
    return seL4_InvalidArgument;
  // Device memory makes frames and untypeds only.
  if (u->device && type != seL4_UntypedObject && size != 12 && size != 21 && size != 30) return seL4_InvalidArgument;
  if (u->device && type == seL4_ARM_PageTableObject) return seL4_InvalidArgument;
  if (type == seL4_UntypedObject && (bits < seL4_MinUntypedBits || bits > seL4_MaxUntypedBits)) return seL4_RangeError;
  seL4_Word one = (seL4_Word)1 << size, at = (u->watermark + one - 1) & ~(one - 1);
  if (at + count * one > u->paddr + ((seL4_Word)1 << u->bits)) return seL4_NotEnoughMemory;
  for (seL4_Word i = 0; i < count; i++)
    if (caps[offset + i].kind != EMPTY) return seL4_DeleteFirst;
  for (seL4_Word i = 0; i < count; i++) {
    struct cap *c = &caps[offset + i];
    c->kind = type == seL4_UntypedObject ? UNTYPED : type == seL4_ARM_PageTableObject ? TABLE : FRAME;
    c->paddr = c->watermark = at + i * one;
    c->bits = size;
    c->device = u->device;
    c->mapped = false;
  }
  u->watermark = at + count * one;
  return seL4_NoError;
}
// The kernel's call itself: parenthesised, past vm.h's macro, which the
// allocator's calls (and the test's own) go through.
seL4_Error (seL4_ARM_Page_Map)(seL4_CPtr frame, seL4_CPtr vspace, seL4_Word va, seL4_CapRights_t rights,
                               seL4_ARM_VMAttributes attributes) {
  (void)rights;
  struct space *s = space_of(vspace);
  struct cap *f = frame < SLOTS ? &caps[frame] : 0;
  if (!s || !f || f->kind != FRAME || f->mapped) return seL4_InvalidCapability;
  if (va & (((seL4_Word)1 << f->bits) - 1)) return seL4_AlignmentError;
  // The levels a frame of this size sits under: a 1 GiB frame in an upper
  // directory, 2 MiB in a directory, 4 KiB in a page table.
  for (unsigned i = 0; i < 3 && spans[i] > f->bits + 8; i++)
    if (!has_table(s, spans[i], va)) return seL4_FailedLookup;
  if (f->bits > 12 && has_table(s, f->bits, va)) return seL4_DeleteFirst; // a table is there
  if (overlaps(s, va, f->bits) || s->leaves == LEAVES) return seL4_DeleteFirst;
  s->leaf[s->leaves][0] = va;
  s->leaf[s->leaves][1] = f->bits;
  s->leaf[s->leaves][2] = attributes;
  s->leaves++;
  f->mapped = true;
  return seL4_NoError;
}
seL4_Error seL4_ARM_PageTable_Map(seL4_CPtr table, seL4_CPtr vspace, seL4_Word va, seL4_ARM_VMAttributes attributes) {
  (void)attributes;
  struct space *s = space_of(vspace);
  if (!s || table >= SLOTS || caps[table].kind != TABLE || caps[table].mapped) return seL4_InvalidCapability;
  // The first missing level, from the top.
  for (unsigned i = 0; i < 3; i++) {
    if (has_table(s, spans[i], va)) continue;
    seL4_Word base = va & ~(((seL4_Word)1 << spans[i]) - 1);
    if (overlaps(s, base, spans[i])) return seL4_DeleteFirst; // a large frame is mapped there
    s->table[s->tables][0] = spans[i];
    s->table[s->tables][1] = va >> spans[i];
    s->tables++;
    caps[table].mapped = true;
    tables_made++;
    return seL4_NoError;
  }
  return seL4_DeleteFirst;
}
seL4_ARM_Page_GetAddress_t seL4_ARM_Page_GetAddress(seL4_CPtr frame) {
  seL4_ARM_Page_GetAddress_t a = {0, 0};
  if (frame >= SLOTS || caps[frame].kind != FRAME) a.error = seL4_InvalidCapability;
  else a.paddr = caps[frame].paddr;
  return a;
}
// stop() yields for ever: the model ends the case there.
static jmp_buf stopped;
static bool may_stop;
void seL4_Yield(void) {
  if (!may_stop) {
    printf("FAILED: the allocator stopped\n");
    exit(1);
  }
  longjmp(stopped, 1);
}

// ---- Boards ----------------------------------------------------------------
static seL4_BootInfo info;
static struct task process;
// Untypeds: RAM (kernel objects) first, then the device memory of `devices`.
static void board(const seL4_UntypedDesc *devices, unsigned n) {
  memset(caps, 0, sizeof caps);
  memset(spaces, 0, sizeof spaces);
  memset(&info, 0, sizeof info);
  tables_made = 0;
  info.untyped.start = 16;
  info.untypedList[0] = (seL4_UntypedDesc){.paddr = 0x40400000, .sizeBits = 22};
  for (unsigned i = 0; i < n; i++) info.untypedList[1 + i] = devices[i];
  info.untyped.end = info.untyped.start + 1 + n;
  for (unsigned i = 0; i <= n; i++) {
    seL4_UntypedDesc d = info.untypedList[i];
    caps[info.untyped.start + i] = (struct cap){UNTYPED, d.paddr, d.paddr, d.sizeBits, d.isDevice, false};
    cursor[i] = d.paddr;
  }
  info.empty.start = FIRST_EMPTY;
  info.empty.end = SLOTS;
  boot = &info;
  ram = info.untyped.start;
  next_slot = info.empty.start;
  caps[3] = (struct cap){.kind = VSPACE, .paddr = 0}; // the supervisor's own
  caps[4] = (struct cap){.kind = VSPACE, .paddr = 1}; // a process's
  memset(&process, 0, sizeof process);
  process.pd = 4;
  mapping = &process;
}
// QEMU virt: device memory from 0x48000000 (the supervisor's windows) and,
// past the RAM QEMU gives the guest, everything up to 2^40.
static const seL4_UntypedDesc qemu[] = {
    {.paddr = 0x48000000, .sizeBits = 27, .isDevice = 1},
    {.paddr = 0x50000000, .sizeBits = 28, .isDevice = 1},
    {.paddr = 0x100000000ull, .sizeBits = 32, .isDevice = 1},
    {.paddr = 0x8000000000ull, .sizeBits = 39, .isDevice = 1},
};

// Every frame of the last window: consecutive slots, ascending addresses, mapped.
static bool window_is(seL4_Word pa, seL4_Word va, unsigned bits) {
  struct space *s = space_of(mapping->pd);
  bool ok = WINDOW_COUNT << window_bits == (seL4_Word)1 << bits;
  for (unsigned k = 0; ok && k < WINDOW_COUNT; k++) {
    struct cap *f = &caps[WINDOW_FIRST + k];
    ok = f->kind == FRAME && f->bits == window_bits && f->paddr == pa + k * WINDOW_STEP && f->mapped && f->device;
    bool found = false;
    for (unsigned i = 0; i < s->leaves; i++)
      if (s->leaf[i][0] == va + k * WINDOW_STEP && s->leaf[i][1] == window_bits &&
          s->leaf[i][2] == seL4_ARM_ExecuteNever)
        found = true;
    ok = ok && found;
  }
  return ok;
}
static bool stops(void (*what)(void)) {
  volatile bool did = true;
  may_stop = true;
  if (setjmp(stopped) == 0) {
    what();
    did = false;
  }
  may_stop = false;
  return did;
}

static void windows(void) {
  board(qemu, 4);
  // A 4 KiB page in an empty space: upper directory, directory, page table.
  EXPECT(map_device(0x49003000, 0x0b004000, 12) == (void *)0x0b004000);
  EXPECT(window_is(0x49003000, 0x0b004000, 12) && window_count == 1);
  EXPECT(tables_made == 3);
  // A 64 KiB socket row: sixteen pages, no new table in the same 2 MiB.
  EXPECT(map_device(0x49230000, 0x0b030000, 16) == (void *)0x0b030000);
  EXPECT(window_is(0x49230000, 0x0b030000, 16) && window_count == 16 && window_bits == 12);
  EXPECT(tables_made == 3);
  // A catalog's 1 MiB that is not 2 MiB aligned: 256 pages, one more table.
  EXPECT(map_device(0x49500000, 0x09500000, 20) == (void *)0x09500000);
  EXPECT(window_is(0x49500000, 0x09500000, 20) && window_count == 256 && window_bits == 12);
  EXPECT(tables_made == 4);
  // Read-only /apps, 16 MiB: eight 2 MiB frames, and no page table under them.
  mapping = &process;
  EXPECT(map_device_rights(0x4a000000, 0x10000000, 24, seL4_CanRead) == (void *)0x10000000);
  EXPECT(window_is(0x4a000000, 0x10000000, 24) && window_count == 8 && window_bits == 21);
  EXPECT(tables_made == 4);
  for (unsigned k = 0; k < 8; k++) EXPECT(!has_table(space_of(4), 21, 0x10000000 + k * 0x200000));
}

static void the_supervisors_own_space(void) {
  // The kernel made the root task's upper directory and directory for its
  // image at 0x100000: mapping beside it makes only a page table.
  board(qemu, 4);
  struct space *own = &spaces[0];
  own->table[0][0] = 39, own->table[0][1] = 0;
  own->table[1][0] = 30, own->table[1][1] = 0;
  own->tables = 2;
  static struct task root;
  root.pd = 3;
  mapping = &root;
  EXPECT(map_device(0x49300000, 0x09100000, 12) == (void *)0x09100000);
  EXPECT(tables_made == 1);
  // A frame the supervisor maps itself (seL4_ARM_Page_Map is vm_page_map here).
  seL4_CPtr page = create(ram, seL4_ARM_SmallPageObject);
  EXPECT(seL4_ARM_Page_Map(page, 3, 0x03000000, seL4_AllRights, seL4_ARM_Default_VMAttributes) == seL4_NoError);
  EXPECT(tables_made == 2);
  // In a new GiB: a directory and a page table.
  seL4_CPtr far = create(ram, seL4_ARM_SmallPageObject);
  EXPECT(seL4_ARM_Page_Map(far, 3, 0x7f000000, seL4_AllRights, seL4_ARM_ExecuteNever) == seL4_NoError);
  EXPECT(tables_made == 4);
  // Refusals other than a missing table come back as they are, no table made.
  EXPECT(seL4_ARM_Page_Map(far, 3, 0x7f001000, seL4_AllRights, seL4_ARM_ExecuteNever) == seL4_InvalidCapability);
  seL4_CPtr odd = create(ram, seL4_ARM_SmallPageObject);
  EXPECT(seL4_ARM_Page_Map(odd, 3, 0x7f000800, seL4_AllRights, seL4_ARM_ExecuteNever) == seL4_AlignmentError);
  EXPECT(tables_made == 4);
}

static void skipping_device_memory(void) {
  board(qemu, 4);
  // From 0x48000000 to 0x49230000: plain untypeds (16 MiB, 2 MiB, 128 KiB, 64 KiB), not 4,656 pages.
  seL4_CPtr before = next_slot;
  map_device(0x49230000, 0x0b030000, 16);
  unsigned fillers = 0;
  for (seL4_CPtr c = before; c < WINDOW_FIRST; c++) fillers += caps[c].kind == UNTYPED;
  EXPECT(fillers == 4);
  EXPECT(caps[before].bits == 24 && caps[before].paddr == 0x48000000);
  // The open-app pool: an untyped of the window itself, after its own fillers.
  seL4_CPtr pool = device_untyped(0x50000000, 27);
  EXPECT(caps[pool].kind == UNTYPED && caps[pool].paddr == 0x50000000 && caps[pool].bits == 27 && caps[pool].device);
  // Above 4 GiB, in an untyped of 2^32 bytes: the cursor and the size are whole words.
  mapping = &process;
  EXPECT(map_device_rights(0x180000000ull, 0x20000000, 21, seL4_AllRights) == (void *)0x20000000);
  EXPECT(window_is(0x180000000ull, 0x20000000, 21) && window_count == 1);
  EXPECT(cursor[3] == 0x180200000ull);
  // And in one of 2^39 bytes, as the kernel reports past QEMU's RAM.
  EXPECT(map_device_rights(0x8040000000ull, 0x40000000, 30, seL4_AllRights) == (void *)0x40000000);
  EXPECT(window_is(0x8040000000ull, 0x40000000, 30) && window_count == 1 && window_bits == 30);
  EXPECT(caps[WINDOW_FIRST - 1].kind == UNTYPED && caps[WINDOW_FIRST - 1].bits == 30);
}

static void earlier_address(void) { map_device(0x49001000, 0x0b005000, 12); }
static void misaligned(void) { map_device(0x49280000, 0x0b008000, 16); }
static void no_device_there(void) { map_device(0x30000000, 0x0b009000, 12); }
static void refusals(void) {
  board(qemu, 4);
  map_device(0x49003000, 0x0b004000, 12);
  // Device memory is consumed in ascending order: an earlier address is gone.
  EXPECT(stops(earlier_address));
  // A 64 KiB window must be 64 KiB aligned at both ends.
  EXPECT(stops(misaligned));
  EXPECT(stops(no_device_there));
}
static void out_of_slots_window(void) { map_device(0x49500000, 0x09500000, 20); }
// Slots run out with a stop, not a retype past the end.
static void out_of_slots(void) {
  board(qemu, 4);
  info.empty.end = next_slot + 100;
  EXPECT(stops(out_of_slots_window));
}

int main(void) {
  windows();
  the_supervisors_own_space();
  skipping_device_memory();
  refusals();
  out_of_slots();
  if (failures) return 1;
  printf("memory.h (AArch64): windows, tables, fillers and refusals as expected\n");
  return 0;
}
