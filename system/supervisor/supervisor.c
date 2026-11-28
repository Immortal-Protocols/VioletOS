// Initial task. Child images have independent code, data, stack and CSpaces.
// Only this process holds untyped memory, child TCBs and their address spaces.
#include "../../platform/sel4/board/memory.h"
#include "tasks.h"
#include "mcs.h"
#include <stddef.h>

#if VM_AARCH64 && defined(VIOLET_MOTO)
// The handset supervisor on AArch64 (targets/moto-g-pure/build64.py): its
// 1 MiB windows are SECTION_PAGES consecutive 4 KiB frames (AArch64 has no
// 1 MiB or 64 KiB frame) and the loader's handoff page is mapped at the same
// address it has on AArch32. The secure-monitor calls (ellis_smc) go to the
// same kernel extension (kernel/sel4/aarch64/ellis-wifi.patch). Camera's 4 MiB
// frame window is two 2 MiB frames there, in its slots 8-9 (camera_frames).
#endif

seL4_IPCBuffer *__sel4_ipc_buffer;
char __sel4_print_error;
extern const unsigned char builtin_catalog[], builtin_catalog_end[];
extern const struct package recovery_manager;
// size_t, as platform/sel4/loader/strings.c defines them: on AArch64 a
// 32-bit length would leave the upper half of the register undefined.
extern int memcmp(const void *, const void *, size_t);
extern void *memset(void *, int, size_t);
extern void *memcpy(void *, const void *, size_t);
enum {
  BASE = 0x100000,
  PAGE = 4096,
  SCRATCH = 0x03000000,
  CAP_BITS = 5, // 32 slots: REPLY_SLOT is 31
  FAULT_SLOT = 15
};
enum {
  LAUNCHER = 1,
  WATCH,
  RUNTIME,
  TOUCH,
  DISPLAY,
  STORAGE,
  RADIO,
  POWER,
  PACKAGES,
  WIFI,
  COUNT = 15
};
enum { GET = 2, EXECUTE = 3, OPEN = 4, QUIT = 10, INVENTORY = 11 };
struct package {
  unsigned magic, abi, id, code, size, crc;
  unsigned char bytes[];
};
struct archive {
  unsigned magic, size, count, crc;
  unsigned char bytes[];
};
struct process {
  struct task task;
  const struct package *package;
  seL4_CPtr ep, frames[64];
  bool running;
};
// Audio follows USB Diagnostics so app and service identities keep their values.
// QEMU has no USB Diagnostics process; its slot stays empty there.
// The Open Runtime (24) hosts Linux-ABI apps from /apps. Slot 23 is left for Bluetooth.
// Camera (25) serves the image sensors to the Open Runtime.
enum { FIRST_WORKER = COUNT, USB_DIAG = COUNT + 6, AUDIO = COUNT + 7, OPEN_RT = COUNT + 9,
       CAMERA = COUNT + 10,
       USB_TRANSFER_BADGE = 14, TOTAL = CAMERA + 2, LANE_BASE = 0x07400000,
       RUNTIME_AUDIO_SLOT = 2, AUDIO_POWER_SLOT = 6, CAMERA_POWER_SLOT = 6,
       // Badges 16 and up mean faults, so Audio's and Camera's supervisor
       // requests use app identities that never hold the lifecycle endpoint.
       AUDIO_LIFE_BADGE = 12, AUDIO_PLATFORM = 0x4071,
       CAMERA_LIFE_BADGE = 13, CAMERA_PLATFORM = 0x4043 };
// The Sentinel (26, system/sentinel/) decides the Open Runtime's capability
// policy in an address space of its own. It gets the receive side of one
// endpoint, which only the Open Runtime can call (slot OPEN_SLOT_SENTINEL
// there), 64 KiB of private heap, and two windows shared with the Open
// Runtime: requests (the runtime writes, the Sentinel reads) and replies (the
// Sentinel writes, the runtime reads). No device but, on QEMU, the root key
// blob's page read-only (its sealing key), and no other endpoint. A catalog
// with an Open Runtime and no Sentinel is refused (parse).
enum { SENTINEL = CAMERA + 1, OPEN_SLOT_SENTINEL = 9, SENTINEL_HEAP = 0x02800000,
       SENTINEL_HEAP_BYTES = 0x10000, SENTINEL_WINDOW_PAGES = 2,
       SENTINEL_REQUEST = 0x09300000, SENTINEL_REPLY = 0x09310000,
       OPEN_SENTINEL_REQUEST = 0x09140000, OPEN_SENTINEL_REPLY = 0x09148000 };
enum { SENTINEL_ROOT_KEY = 0x09320000 };
// The launcher, watchdog, Runtime and catalog apps end below their two-page
// stack at 0x13e000. The Open Runtime's stack is at OPEN_STACK_TOP instead, so
// its image may run on up to 0x180000, well below its IPC buffer at 0x200000.
// The services (own_stack) keep their stacks out of the image too, so they get
// the same SERVICE_WINDOW.
enum { OPEN_WINDOW = 0x80000, SERVICE_WINDOW = 0x80000 };
// The Open Runtime's CSpace and the capabilities the supervisor places in it.
// Runtime knows it as client 9, an identity no catalog app uses.
// OPEN_BITS: a slot for each 4 KiB page of its pool and one more for each
// page an app has mapped (runtime/open/sys.rs CNODE_BITS, which must agree):
// 2^18 for the emulator's 384 MiB pool (a 4 MiB table, 8 MiB on AArch64,
// whose slots are 32 bytes), 2^17 for the handset's 256 MiB (2 MiB).
#ifdef VIOLET_QEMU
enum { OPEN_BITS = 18 };
#else
enum { OPEN_BITS = 17 };
#endif
enum { OPEN_RUNTIME_BADGE = 9, OPEN_STACK_TOP = 0x02c00000, OPEN_STACK_PAGES = 16,
       OPEN_SLOT_RUNTIME = 2, OPEN_SLOT_CNODE = 3, OPEN_SLOT_PD = 4, OPEN_SLOT_ASID = 5,
       OPEN_SLOT_KERNEL = 6, OPEN_SLOT_AUTHORITY = 7, OPEN_SLOT_TIMER = 10, OPEN_SLOT_WAKE = 11,
       OPEN_SLOT_SCHED_CONTROL = 13, OPEN_SLOT_SCHED_CORES = 20,
       OPEN_SLOT_POOL = 16,
       // The Open Runtime's priority while it waits for events (it starts at the
       // 100 every process runs at), and the badge its timer signals with.
       OPEN_EVENT_PRIORITY = 150, OPEN_TIMER_BADGE = 1u << 27, OPEN_TIMER_IRQ = 30 };
// Each resident app needs 512 KiB, with room left over for shared runtime
// objects: 8 MiB holds the Open Runtime's twelve app slices (runtime/open/mod.rs
// MAX_APPS) on both boards. The Open Runtime carves as many as it is given, so
// a phone without a spare 8 MiB untyped gets 4 MiB and seven apps instead.
enum { OPEN_KERNEL_BITS = 23 };
// The supervisor's own Reply object (MCS), for answering on `life`.
static seL4_CPtr root_reply;
#ifdef CONFIG_KERNEL_MCS
// Every thread runs on a scheduling context of its own: by default a full
// 10 ms budget every 10 ms, which is round robin with the 10 ms timeslice the
// kernel used before MCS. The Open Runtime gives app threads other budgets.
enum { ROUND_ROBIN_US = 10000 };
static void give_time(seL4_CPtr tcb, unsigned priority, seL4_CPtr fault_ep) {
  seL4_CPtr sc = allocate(ram, seL4_SchedContextObject, seL4_MinSchedContextBits);
  check(seL4_SchedControl_ConfigureFlags(boot->schedcontrol.start, sc, ROUND_ROBIN_US, ROUND_ROBIN_US, 0, 0, 0));
  check(seL4_TCB_SetSchedParams(tcb, seL4_CapInitThreadTCB, 0, priority, sc, fault_ep));
}
#endif
// The Open Runtime's timer interrupt handler and Touch's (handset), deleted
// before the system is rebuilt.
static seL4_CPtr open_timer_handler, touch_irq_handler;
enum { TOUCH_GIC_IRQ = 203, TOUCH_IRQ_BADGE = 1u << 20, OPEN_TOUCH_BADGE = 1u << 26,
       TOUCH_SLOT_IRQ = 9, TOUCH_SLOT_OPEN = 10, TOUCH_SLOT_TCB = 11 };
static struct process procs[TOTAL];
static seL4_CPtr life, scratch_slot;
static volatile unsigned *report;
static unsigned generation, source;
unsigned crc32(const unsigned char *b, unsigned n) {
  unsigned v = ~0u;
  while (n--) {
    v ^= *b++;
    for (unsigned j = 0; j < 8; j++)
      v = (v >> 1) ^ ((0 - (v & 1)) & 0xedb88320);
  }
  return ~v;
}
// Every package is a VPK2 record: header, image, then an 80-byte signature
// trailer (image/packages/sign.py). package_verify (system/package-manager/
// verify.rs) returns the record's security version, or 0 unless a key pinned
// in this image signed its header, image and version. Unsigned packages do
// not run: there is no fallback. The floors refuse older signed packages.
enum { SIGNATURE = 80, FLOOR_RECORD = 8 + 8 * 32 };
extern unsigned package_verify(const void *package, unsigned limit);
extern void package_floor_factory(unsigned id, unsigned version);
extern void package_floor_begin(void);
extern void package_floor_ran(unsigned id, unsigned version);
extern bool package_floor_boots(unsigned id, unsigned version);
extern bool package_floor_installs(unsigned id, unsigned version);
// A catalog this supervisor wrote may end with a floor record: the highest
// version seen of each component, so removal and a reboot do not forget it.
extern unsigned package_floor_record_floor(const unsigned char *record, unsigned bytes, unsigned id);
extern void package_floor_load(const unsigned char *record, unsigned bytes);
extern unsigned package_floor_record(unsigned char *out, unsigned room, unsigned id, unsigned version);
// A fault rejects an installed catalog only during its trial: from its commit
// until it has run healthy for the health period (verify.rs, Trial).
extern void package_trial_installed(void);
extern void package_trial_healthy(void);
extern bool package_trial_fault_rejects(void);
// Full-system updates (system/package-manager/ota.rs, verify.rs): a signed
// manifest for the image Storage wrote and read back in the inactive slot.
extern unsigned ota_verify(const unsigned char *manifest, unsigned slot_sectors, const unsigned char *digest,
                           unsigned *sectors, unsigned *why);
extern unsigned ota_system_version(void);
// When to ask Storage to mark the running slot successful (ota.rs, Mark):
// only after a healthy period since the last (re)start, retried until it works.
extern void ota_mark_restart(void);
extern bool ota_mark_due(bool healthy, unsigned now);
extern void ota_mark_result(bool ok, unsigned now, unsigned second);
// The floor record of the catalog parse() accepted last.
static const unsigned char *catalog_floors;
static unsigned catalog_floor_bytes;
// Only for a package valid_package has accepted.
static unsigned package_version(const struct package *p) {
  unsigned version;
  memcpy(&version, p->bytes + p->size + 8, 4);
  return version;
}

// Services whose stack is kept out of their image (stack_top), so their
// package may fill SERVICE_WINDOW. Everyone else keeps the 0x3e000 window
// below the stack at 0x13e000 (Runtime: 0x38000, under its larger stack).
static bool own_stack(unsigned id) {
  return id == TOUCH || id == DISPLAY || id == STORAGE || id == RADIO || id == POWER ||
         id == USB_DIAG || id == AUDIO || id == CAMERA || id == SENTINEL;
}
static unsigned package_window(unsigned id) {
  return id == RUNTIME ? 0x38000 : id == OPEN_RT ? OPEN_WINDOW : own_stack(id) ? SERVICE_WINDOW : 0x3e000;
}
// A package's signed ABI word names its architecture in the top byte
// (image/packages/package_format.py ARCH_ABI): none for AArch32, 0x64 for
// AArch64. Only exact words of this supervisor's own architecture are accepted.
#define ABI_ARCH (VM_AARCH64 ? 0x64000000u : 0u)
static bool valid_package(const struct package *p, unsigned limit) {
  if (limit < sizeof(*p) + SIGNATURE) return false;
  bool abi_ok=p->abi==((p->id==RUNTIME ? 2u : 1u) | ABI_ARCH);
#ifdef VIOLET_MOTO
  // The handset's Radio: ABI 3 has 128 KiB of private RAM, ABI 4 has 192 KiB.
  if (p->id==RADIO && (p->abi==(3|ABI_ARCH) || p->abi==(4|ABI_ARCH))) abi_ok=true;
#endif
  bool allowed_id = p->id < COUNT || p->id == AUDIO || p->id == OPEN_RT || p->id == CAMERA;
  allowed_id |= p->id == SENTINEL;
#ifdef VIOLET_MOTO
  allowed_id |= p->id == USB_DIAG;
#endif
  if (p->magic != 0x324b5056 ||
      !abi_ok || !p->id || p->id == PACKAGES ||
      !allowed_id || !p->code || p->code % PAGE || p->size < p->code ||
      p->size > limit - sizeof(*p) - SIGNATURE ||
      p->size > package_window(p->id))
    return false;
  if (p->id >= 11 && p->id < COUNT) {
    if (p->size > 65512 || p->size < p->code + 20 ||
        memcmp(p->bytes + p->size - 20, "APP1", 4))
      return false;
    const unsigned char *name = p->bytes + p->size - 16;
    bool ended = false;
    if (!name[0])
      return false;
    for (unsigned i = 0; i < 16; i++) {
      unsigned c = name[i];
      if (!c)
        ended = true;
      else if (ended || !((c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') ||
                          c == ' ' || c == '-'))
        return false;
    }
  }
  return crc32(p->bytes, p->size) == p->crc &&
         package_verify(p, sizeof(*p) + p->size + SIGNATURE) != 0;
}
static bool parse(const struct archive *a, unsigned limit) {
  if (a->magic != 0x31544356 || limit < 16 || a->size > limit - 16 ||
      a->count > COUNT || crc32(a->bytes, a->size) != a->crc)
    return false;
  const struct package *found[TOTAL] = {0};
  unsigned pos = 0;
  for (unsigned i = 0; i < a->count; i++) {
    if (a->size - pos < sizeof(struct package))
      return false;
    const struct package *p = (const void *)(a->bytes + pos);
    if (!valid_package(p, a->size - pos) || found[p->id])
      return false;
    // The boot image's own catalog sets each version floor; any other catalog must meet it.
    if ((const void *)a == (const void *)builtin_catalog)
      package_floor_factory(p->id, package_version(p));
    else if (!package_floor_boots(p->id, package_version(p)))
      return false;
    pos += (sizeof(*p) + p->size + SIGNATURE + 3) & ~3u;
    if (pos > a->size)
      return false;
    found[p->id] = p;
  }
#ifdef VIOLET_MOTO
  if (!found[STORAGE] || !found[RADIO] || !found[POWER]) return false;
#endif
  // Only a well-formed floor record may follow the packages, never in the
  // boot image's own catalog, and no package may be below its own catalog's floor.
  const unsigned char *floors = a->bytes + pos;
  unsigned floor_bytes = a->size - pos;
  if (floor_bytes) {
    if ((const void *)a == (const void *)builtin_catalog ||
        package_floor_record_floor(floors, floor_bytes, 0) == ~0u)
      return false;
    for (unsigned i = 1; i < TOTAL; i++)
      if (found[i] && package_version(found[i]) < package_floor_record_floor(floors, floor_bytes, i))
        return false;
  }
  for (unsigned i = 1; i < COUNT; i++)
    if ((i == LAUNCHER || i == RUNTIME || i == TOUCH || i == DISPLAY) &&
        !found[i])
      return false;
  // This Open Runtime asks the Sentinel for every decision (system/sentinel).
  // A catalog with an Open Runtime and no Sentinel (one from before it) is
  // refused whole, so the boot falls back to the next catalog (in the end
  // the factory's) instead of starting with no open apps.
  if (found[OPEN_RT] && !found[SENTINEL])
    return false;
  catalog_floors = floor_bytes ? floors : 0;
  catalog_floor_bytes = floor_bytes;
  for (unsigned i = 1; i < COUNT; i++)
    procs[i].package = found[i];
#ifdef VIOLET_MOTO
  // Older installed catalogs inherit Diagnostics from the factory catalog.
  if (found[USB_DIAG]) procs[USB_DIAG].package = found[USB_DIAG];
#endif
  procs[AUDIO].package = found[AUDIO];
  procs[OPEN_RT].package = found[OPEN_RT];
  procs[CAMERA].package = found[CAMERA];
  procs[SENTINEL].package = found[SENTINEL];
  return true;
}
static void grant(struct task *t, unsigned slot, seL4_CPtr ep, unsigned badge,
                  bool server) {
  check(seL4_CNode_Mint(t->caps, slot, t->cap_bits ? t->cap_bits : CAP_BITS, seL4_CapInitThreadCNode, ep,
                        SEL4_DEPTH, seL4_CapRights_new(!server, 0, server, !server),
                        badge));
}
static void fill_frame(seL4_CPtr frame, const unsigned char *data, unsigned n) {
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, scratch_slot, SEL4_DEPTH,
                        seL4_CapInitThreadCNode, frame, SEL4_DEPTH, seL4_AllRights));
  check(seL4_ARM_Page_Map(scratch_slot, bootstrap.pd, SCRATCH, seL4_AllRights,
                          seL4_ARM_Default_VMAttributes));
  if (n)
    memcpy((void *)SCRATCH, data, n);
  memset((void *)(uintptr_t)(SCRATCH + n), 0, PAGE - n);
  check(seL4_ARM_Page_Unify_Instruction(scratch_slot, 0, PAGE));
  check(seL4_ARM_Page_Unmap(scratch_slot));
  check(seL4_CNode_Delete(seL4_CapInitThreadCNode, scratch_slot, SEL4_DEPTH));
}
static void private_page(struct process *p, unsigned va,
                         const unsigned char *data, unsigned n, bool exec) {
  struct task *t = &p->task;
  seL4_CPtr frame = create(ram, seL4_ARM_SmallPageObject);
  if (n)
    fill_frame(frame, data, n);
  ensure_table(t, va);
  check(seL4_ARM_Page_Map(frame, t->pd, va, seL4_CapRights_new(0, 0, 1, !exec),
                          seL4_ARM_Default_VMAttributes |
                              (exec ? 0 : seL4_ARM_ExecuteNever)));
  if (va == IPC_BUFFER)
    t->ipc = frame;
  else if (va >= BASE && va < STACK_TOP && (va - BASE) / PAGE < 64)
    p->frames[(va - BASE) / PAGE] = frame;
}
// Storage uses nested 512-byte sector buffers. Keep its stack separate from
// the package image, with an unmapped guard page below it. Radio's stack is
// separate too, and so is every other own_stack service's (two pages at
// SERVICE_STACK_TOP, clear of the private RAM at 0x02800000), so their
// packages may use the whole SERVICE_WINDOW.
enum { STORAGE_STACK_TOP = 0x02400000, STORAGE_STACK_PAGES = 8 };
enum { RADIO_STACK_TOP = 0x02600000, RADIO_STACK_PAGES = 8 };
enum { SERVICE_STACK_TOP = 0x02a00000 };
static unsigned stack_top(unsigned id) {
  return id == STORAGE ? STORAGE_STACK_TOP : id == RADIO ? RADIO_STACK_TOP
         : id == OPEN_RT ? OPEN_STACK_TOP : own_stack(id) ? SERVICE_STACK_TOP : STACK_TOP;
}
static seL4_CPtr spare_ram(unsigned type, unsigned bits);
static void prepare_process(unsigned id) {
  struct process *p = &procs[id];
  const struct package *pkg = p->package;
  if (!pkg)
    return;
  struct task *t = &p->task;
  t->pd = create(ram, VM_VSPACE_OBJECT);
  check(seL4_ARM_ASIDPool_Assign(seL4_CapInitThreadASIDPool, t->pd));
  t->cap_bits = id == OPEN_RT ? OPEN_BITS : CAP_BITS;
  t->caps = id == OPEN_RT ? spare_ram(seL4_CapTableObject, t->cap_bits) : 0;
  if (!t->caps) {
    // Without room for its table the Open Runtime starts with a minimal one and reports it.
    t->cap_bits = CAP_BITS;
    t->caps = allocate(ram, seL4_CapTableObject, CAP_BITS);
  }
  t->thread = create(ram, seL4_TCBObject);
  for (unsigned off = 0; off < pkg->size; off += PAGE) {
    unsigned n = pkg->size - off;
    private_page(p, BASE + off, pkg->bytes + off, n > PAGE ? PAGE : n,
                 off < pkg->code);
  }
  if (id == RUNTIME) {
    check(pkg->size > 0x38000);
    for (unsigned i = 3; i <= 8; i++)
      private_page(p, STACK_TOP - i * PAGE, 0, 0, false);
  }
  // Audio keeps the microphone recording in 128 KiB of zeroed private RAM.
  if (id == AUDIO)
    for (unsigned off = 0; off < 0x20000; off += PAGE) private_page(p, 0x02800000 + off, 0, 0, false);
  // The Sentinel's heap: zeroed, private.
  if (id == SENTINEL)
    for (unsigned off = 0; off < SENTINEL_HEAP_BYTES; off += PAGE) private_page(p, SENTINEL_HEAP + off, 0, 0, false);
#ifdef VIOLET_MOTO
  if (pkg->id==RADIO && (pkg->abi==(3|ABI_ARCH) || pkg->abi==(4|ABI_ARCH)))
    for (unsigned off=0;off<(pkg->abi==(4|ABI_ARCH) ? 0x30000u : 0x20000u);off+=PAGE) private_page(p,0x02800000+off,0,0,false);
#endif
  if (id == STORAGE) {
    for (unsigned i = 1; i <= STORAGE_STACK_PAGES; i++)
      private_page(p, STORAGE_STACK_TOP - i * PAGE, 0, 0, false);
  } else if (id == RADIO) {
    for (unsigned i = 1; i <= RADIO_STACK_PAGES; i++)
      private_page(p, RADIO_STACK_TOP - i * PAGE, 0, 0, false);
  } else if (id == OPEN_RT) {
    for (unsigned i = 1; i <= OPEN_STACK_PAGES; i++)
      private_page(p, OPEN_STACK_TOP - i * PAGE, 0, 0, false);
  } else {
    private_page(p, stack_top(id) - 2 * PAGE, 0, 0, false);
    private_page(p, stack_top(id) - PAGE, 0, 0, false);
  }
  private_page(p, IPC_BUFFER, 0, 0, false);
  grant(t, FAULT_SLOT, life, 16 + id, false);
  seL4_Word guard = seL4_CNode_CapData_new(0, SEL4_DEPTH - t->cap_bits).words[0];
#ifdef CONFIG_KERNEL_MCS
  check(seL4_TCB_Configure(t->thread, t->caps, guard, t->pd, 0, IPC_BUFFER, t->ipc));
  // The fault endpoint is named in this CSpace; a Reply object for receiving.
  seL4_CPtr fault = next_slot++;
  check(seL4_CNode_Mint(seL4_CapInitThreadCNode, fault, SEL4_DEPTH, seL4_CapInitThreadCNode, life, SEL4_DEPTH,
                        seL4_CapRights_new(1, 0, 0, 1), 16 + id));
  give_time(t->thread, 100, fault);
  seL4_CPtr reply = create(ram, seL4_ReplyObject);
  check(seL4_CNode_Move(t->caps, REPLY_SLOT, t->cap_bits, seL4_CapInitThreadCNode, reply, SEL4_DEPTH));
#else
  check(seL4_TCB_Configure(t->thread, FAULT_SLOT, t->caps, guard, t->pd, 0,
                           IPC_BUFFER, t->ipc));
  check(seL4_TCB_SetPriority(t->thread, seL4_CapInitThreadTCB, 100));
#endif
}
// The timekeeper (timekeeper_thread.h): one thread that owns the user timer,
// so a process's delay blocks instead of spinning and the CPU can idle.
#include "timekeeper_thread.h"
static void start(unsigned id) {
  struct process *p = &procs[id];
  if (p->running)
    return;
  // r1 (x1): TK_LIVE if the process may sleep on the timekeeper (runtime/transport.c).
  seL4_UserContext regs = {.pc = BASE, .sp = stack_top(id), .VM_ARG0 = id, .VM_ARG1 = timekeeper_token(id)};
  check(seL4_TCB_WriteRegisters(p->task.thread, 1, 0,
                                sizeof(regs) / sizeof(seL4_Word), &regs));
  p->running = true;
}
static void quit_app(unsigned id) {
  struct process *p = &procs[id];
  if (!p->running)
    return;
  check(seL4_TCB_Suspend(p->task.thread));
  for (unsigned off = p->package->code; off < p->package->size; off += PAGE) {
    unsigned n = p->package->size - off;
    fill_frame(p->frames[off / PAGE], p->package->bytes + off,
               n > PAGE ? PAGE : n);
  }
  for (unsigned i = 62; i < 64; i++)
    fill_frame(p->frames[i], 0, 0);
  fill_frame(p->task.ipc, 0, 0);
  p->running = false;
}
static void surface(unsigned id) {
  struct task *display = &procs[DISPLAY].task;
  ensure_table(display, 0x06000000 + (id - 1) * 0x100000);
  struct task *app = &procs[id].task;
  if (app->pd)
    ensure_table(app, 0x06000000);
  // 64 KiB frames (4 KiB pages on AArch64, vm.h).
  for (unsigned off = 0; off < 240 * 400 * 4; off += VM_64K_STEP) {
    seL4_CPtr f = create(ram, VM_64K_OBJECT);
    check(seL4_ARM_Page_Map(
        f, display->pd, 0x06000000 + (id - 1) * 0x100000 + off,
        seL4_CapRights_new(0, 0, 1, 0),
        seL4_ARM_Default_VMAttributes | seL4_ARM_ExecuteNever));
    if (app->pd) {
      seL4_CPtr copy = next_slot++;
      check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, SEL4_DEPTH,
                            seL4_CapInitThreadCNode, f, SEL4_DEPTH, seL4_AllRights));
      check(seL4_ARM_Page_Map(copy, app->pd, 0x06000000 + off, seL4_AllRights,
                              seL4_ARM_Default_VMAttributes |
                                  seL4_ARM_ExecuteNever));
    }
  }
}
// The last frame the last map_device call made. On AArch32 that is the last
// capability made; on AArch64 a mapping may make page tables after its frames.
#define LAST_FRAME (VM_AARCH64 ? WINDOW_FIRST + WINDOW_COUNT - 1 : next_slot - 1)
static void *device(unsigned id, unsigned pa, unsigned va, unsigned bits) {
  mapping = id ? &procs[id].task : &bootstrap;
  return map_device(pa, va, bits);
}
#ifdef VIOLET_MOTO
// Share an already allocated device frame with the supervisor without
// allocating the same physical address from its untyped a second time.
static void board_device(unsigned id, unsigned pa, unsigned va, unsigned root_va) {
  device(id, pa, va, 12);
  unsigned frame = LAST_FRAME;
  ensure_table(&bootstrap, root_va);
  unsigned copy = next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, SEL4_DEPTH,
                        seL4_CapInitThreadCNode, frame, SEL4_DEPTH, seL4_AllRights));
  check(seL4_ARM_Page_Map(copy, bootstrap.pd, root_va, seL4_AllRights, seL4_ARM_ExecuteNever));
}
#endif
static void reset_allocator(void) {
  memset(&bootstrap, 0, sizeof(bootstrap));
  memset(procs, 0, sizeof(procs));
  next_slot = boot->empty.start;
  bootstrap.pd = seL4_CapInitThreadVSpace;
  unsigned largest = 0;
  for (unsigned i = 0; i < boot->untyped.end - boot->untyped.start; i++) {
    seL4_UntypedDesc d = boot->untypedList[i];
    cursor[i] = d.paddr;
    if (!d.isDevice && d.sizeBits > largest) {
      largest = d.sizeBits;
      ram = boot->untyped.start + i;
    }
  }
  ensure_table(&bootstrap, SCRATCH);
  scratch_slot = next_slot++;
}
#ifdef VIOLET_MOTO
extern unsigned record_boot_stage(unsigned char stage);
static seL4_CPtr boot_display_frame;
static volatile unsigned boot_stages;
static void boot_stage(unsigned stage) {
  // Each completed stage leaves one green block above the loader's white bar.
  boot_stages |= 1u << stage;
  volatile unsigned *pixels = (void *)0x07900000;
  for (unsigned y = 4; y < 16; y++)
    for (unsigned x = 0; x < 24; x++)
      pixels[y * 720 + stage * 32 + x] = 0xff38df82;
  __sync_synchronize();
}
#endif
static void build_group(void) {
  reset_allocator();
#ifdef VIOLET_MOTO
  device(0, 0x79450000, 0x07900000, 16);
  // On AArch64 the 64 KiB are WINDOW_COUNT pages from WINDOW_FIRST.
  boot_display_frame = VM_AARCH64 ? WINDOW_FIRST : next_slot - 1;
  boot_stage(0);
#endif
  const struct archive *factory = (const void *)builtin_catalog;
  check(!parse(factory, builtin_catalog_end - builtin_catalog));
}
#ifdef VIOLET_QEMU
struct device_page {
  unsigned id, pa, va;
  seL4_CPtr frame;
  // 16 for a 64 KiB frame; 0 is a 4 KiB page.
  unsigned bits;
#if VM_AARCH64
  // What map_device made of it: `pieces` frames of 2^piece_bits from `frame`
  // (sixteen pages for a 64 KiB row: AArch64 has no 64 KiB frame).
  unsigned pieces, piece_bits;
#endif
};
static struct device_page devices[] = {
    {TOUCH, 0x09000000, 0x09003000, 0},   {RADIO, 0x09010000, 0x09004000, 0},
    {STORAGE, 0x0a000000, 0x0a000000, 0}, {RADIO, 0x0a001000, 0x09001000, 0},
    {RADIO, 0x0a002000, 0x09003000, 0},   {0, 0x0a003000, 0x0a003000, 0},
    {TOUCH, 0x49000000, 0x09005000, 0},   {RADIO, 0x49001000, 0x09000000, 0},
    {POWER, 0x49002000, 0x09000000, 0},
    // The device root key blob (tools/root_key.py), for Storage alone.
    {STORAGE, 0x49003000, 0x0b004000, 0},
    {STORAGE, 0x49200000, 0x0b000000, 0},
    {RADIO, 0x49201000, 0x09002000, 0},   {RADIO, 0x49202000, 0x09005000, 0},
    {0, 0x49203000, 0x0b003000, 0},
    // Emulated sound card: control page, an eight-page PCM ring,
    {AUDIO, 0x49204000, 0x09000000, 0}, {AUDIO, 0x49205000, 0x09001000, 0},
    {AUDIO, 0x49206000, 0x09002000, 0}, {AUDIO, 0x49207000, 0x09003000, 0},
    {AUDIO, 0x49208000, 0x09004000, 0}, {AUDIO, 0x49209000, 0x09005000, 0},
    {AUDIO, 0x4920a000, 0x09006000, 0}, {AUDIO, 0x4920b000, 0x09007000, 0},
    {AUDIO, 0x4920c000, 0x09008000, 0},
    // and the two-page microphone ring.
    {AUDIO, 0x4920d000, 0x09009000, 0}, {AUDIO, 0x4920e000, 0x0900a000, 0},
    // Open Runtime log ring: what open apps write to stdout and stderr.
    {OPEN_RT, 0x49220000, 0x09000000, 0},
    // Open apps' sockets ("NET2", runtime/open/net.rs), served by the emulator's
    // host relay (tools/net_relay.py): 0xd0000 bytes from 0x49230000, at
    // 0x09400000 in the Open Runtime, in 64 KiB frames except beside the
    // Bluetooth pages. First the control page, the slot headers and nine
    // slots' rings (tools/test_net_layout.py checks these rows).
    {OPEN_RT, 0x49230000, 0x09400000, 0, 16},
    {OPEN_RT, 0x49240000, 0x09410000, 0, 16},
    {OPEN_RT, 0x49250000, 0x09420000, 0, 16},
    {OPEN_RT, 0x49260000, 0x09430000, 0, 16},
    // Bluetooth HCI (runtime/open/hci.rs), served by the emulator's virtual
    // controller (tools/bluetooth_controller.py).
    {OPEN_RT, 0x49270000, 0x09440000, 0},
    {OPEN_RT, 0x49271000, 0x09441000, 0},
    // The other 23 slots' rings, from 0x49273000.
    {OPEN_RT, 0x49273000, 0x09443000, 0},
    {OPEN_RT, 0x49274000, 0x09444000, 0},
    {OPEN_RT, 0x49275000, 0x09445000, 0},
    {OPEN_RT, 0x49276000, 0x09446000, 0},
    {OPEN_RT, 0x49277000, 0x09447000, 0},
    {OPEN_RT, 0x49278000, 0x09448000, 0},
    {OPEN_RT, 0x49279000, 0x09449000, 0},
    {OPEN_RT, 0x4927a000, 0x0944a000, 0},
    {OPEN_RT, 0x4927b000, 0x0944b000, 0},
    {OPEN_RT, 0x4927c000, 0x0944c000, 0},
    {OPEN_RT, 0x4927d000, 0x0944d000, 0},
    {OPEN_RT, 0x4927e000, 0x0944e000, 0},
    {OPEN_RT, 0x4927f000, 0x0944f000, 0},
    {OPEN_RT, 0x49280000, 0x09450000, 0, 16},
    {OPEN_RT, 0x49290000, 0x09460000, 0, 16},
    {OPEN_RT, 0x492a0000, 0x09470000, 0, 16},
    {OPEN_RT, 0x492b0000, 0x09480000, 0, 16},
    {OPEN_RT, 0x492c0000, 0x09490000, 0, 16},
    {OPEN_RT, 0x492d0000, 0x094a0000, 0, 16},
    {OPEN_RT, 0x492e0000, 0x094b0000, 0, 16},
    {OPEN_RT, 0x492f0000, 0x094c0000, 0, 16}};
#else
static unsigned handset_report[6];
static unsigned uploaded, upload_size, upload_crc;
// The composed catalog Storage commits (two sections, read-only in Storage
// at 0x0d100000), and the USB trial's copy of a candidate package, which must
// outlive a group teardown: in the device section after the installed
// catalog's 1.25 MiB, below the loader's bank word at its last word.
static seL4_CPtr catalog_candidate_frame[2];
static unsigned char *usb_scratch;
static const struct package *factory_usb;
static bool usb_factory_only;
static unsigned usb_trial_bytes, usb_trial_generation;
enum usb_failure { USB_HEALTHY, USB_FAULT, USB_TIMEOUT, USB_DEPENDENCY_FAULT };
static enum usb_failure usb_last_failure;
// Kept in the read-only image so virtual-board tests can shorten the trial.
static const volatile unsigned usb_trial_timeout_ms = 45000;
static unsigned long long usb_deadline;
static unsigned long long ticks64(void);
#endif
extern unsigned volume_load(void *destination);
extern bool volume_commit(const void *data, unsigned length, unsigned active);
extern bool volume_reject(unsigned active);
static unsigned volume_bank;
// The largest catalog this supervisor boots or composes: 1 MiB on QEMU (its
// staged and installed windows are single 1 MiB frames), 1.25 MiB on the
// handset (image/packages/package_format.py HANDSET_LIMIT, catalog_volume.rs).
#ifdef VIOLET_MOTO
enum { CATALOG_LIMIT = 0x140000 };
#else
enum { CATALOG_LIMIT = 1 << 20 };
#endif
static const struct package *inbox;
static struct archive *local_catalog, *new_catalog;
#ifdef VIOLET_MOTO
#ifdef VIOLET_DEVELOPER_KEY
#include "developer-key.h"
// An ASN.1 P-256 signature by `key` over the SHA-256 of `data` (verify.rs).
extern bool package_detached_verify(const unsigned char *key, const void *data, unsigned size,
                                    const unsigned char *der, unsigned len);
#endif
// USB provisioning lives only in supervisor RAM. Only Radio may retrieve it.
static unsigned char profile_name[32], profile_key[32];
static unsigned profile_length, profile_transaction, profile_epoch;
static unsigned long long profile_ticks;
static bool profile_pending, profile_ready;
static unsigned join_job;
static unsigned char package_signature[72];
static bool signature_started, package_authorized;
static bool signed_update(unsigned size) {
#ifndef VIOLET_DEVELOPER_KEY
  // A release image pins no developer key, so nothing is activated over USB:
  // packages are installed only by confirmation on the handset (Apps, Install
  // Package), and USB Diagnostics, which that path excludes, changes only
  // with the boot image.
  (void)size;
  (void)package_signature;
  return false;
#else
  if (!upload_size || uploaded != upload_size || (inbox->id != RADIO && inbox->id != USB_DIAG) ||
      !valid_package(inbox, uploaded)) return false;
  _Static_assert(sizeof(developer_key) == 65, "an uncompressed P-256 point");
  return package_detached_verify(developer_key, inbox, uploaded, package_signature, size);
#endif
}
#endif
static bool valid_inbox(void) {
#ifdef VIOLET_MOTO
  return upload_size && uploaded == upload_size && valid_package(inbox, upload_size);
#else
  return valid_package(inbox, 1 << 20);
#endif
}
static const char *title(const struct package *p) {
  static const char names[][16] = {"",      "LAUNCHER", "STOPWATCH",  "RUNTIME",
                                   "TOUCH", "DISPLAY",  "STORAGE",    "RADIO",
                                   "POWER", "",         "WI-FI SETUP"};
  static const char diag_name[16] = "USB DIAGNOSTICS", audio_name[16] = "AUDIO",
                    open_name[16] = "OPEN RUNTIME", camera_name[16] = "CAMERA";
  if (p->id == USB_DIAG) return diag_name;
  if (p->id == CAMERA) return camera_name;
  if (p->id == SENTINEL) return "SENTINEL\0\0\0\0\0\0\0";
  if (p->id == AUDIO) return audio_name;
  if (p->id == OPEN_RT) return open_name;
  return p->id >= 11 ? (const char *)p->bytes + p->size - 16 : names[p->id];
}
static void reply_title(const struct package *p) {
  const char *name = title(p);
  for (unsigned i = 0; i < 4; i++) {
    unsigned word;
    memcpy(&word, name + i * 4, 4);
    seL4_SetMR(4 + i, word);
  }
}
static bool update_catalog(unsigned id, bool remove) {
#ifdef VIOLET_MOTO
  // Each physical device is wired to a process on this board.
  if (remove && id >= STORAGE && id <= POWER) return false;
#endif
  bool known = (id > 0 && id < COUNT && id != PACKAGES) || id == AUDIO || id == OPEN_RT || id == CAMERA;
  known |= id == SENTINEL;
  // The Open Runtime does not run without the Sentinel (parse): it stays.
  if (remove && id == SENTINEL && procs[OPEN_RT].package) return false;
#ifdef VIOLET_MOTO
  if (usb_trial_bytes && id != USB_DIAG) return false;
  if (id == USB_DIAG) known = !remove && usb_trial_bytes && usb_trial_generation == generation;
#endif
  if (!known)
    return false;
  if (remove && (!procs[id].package || id == LAUNCHER || id == RUNTIME ||
                 id == TOUCH || id == DISPLAY))
    return false;
  if (!remove && (!valid_inbox() || inbox->id != id))
    return false;
  // No rollback: nothing older than the version running, one that ran healthy
  // since power-on, or one this catalog's floor record remembers.
  if (!remove && !package_floor_installs(id, package_version(inbox)))
    return false;
  unsigned count = 0, length = 0;
  for (unsigned i = 1; i < TOTAL; i++) {
    if (i >= COUNT && i != USB_DIAG && i != AUDIO && i != OPEN_RT && i != CAMERA && i != SENTINEL) continue;
    const struct package *p = i == id ? (remove ? 0 : inbox) : procs[i].package;
    if (!p || i == PACKAGES)
      continue;
    unsigned bytes = sizeof(*p) + p->size + SIGNATURE, aligned = (bytes + 3) & ~3u;
    if (length + aligned + 16 > CATALOG_LIMIT)
      return false;
    memcpy(new_catalog->bytes + length, p, bytes);
    memset(new_catalog->bytes + length + bytes, 0, aligned - bytes);
    length += aligned;
    count++;
  }
  // The floor record keeps the versions seen, including the one being
  // removed or replaced here, in the catalog itself.
  unsigned char floors[FLOOR_RECORD];
  unsigned floor_bytes = package_floor_record(floors, sizeof(floors), id, remove ? 0 : package_version(inbox));
  if (floor_bytes > sizeof(floors) || length + floor_bytes + 16 > CATALOG_LIMIT)
    return false;
  memcpy(new_catalog->bytes + length, floors, floor_bytes);
  length += floor_bytes;
  new_catalog->magic = 0x31544356;
  new_catalog->size = length;
  new_catalog->count = count;
  new_catalog->crc = crc32(new_catalog->bytes, length);
#ifdef VIOLET_MOTO
  if (!procs[STORAGE].ep || (remove && id == STORAGE)) return false;
  seL4_SetMR(0,0x56434d54);
  seL4_SetMR(1,0);
  seL4_SetMR(2,length+16);
  seL4_SetMR(3,volume_bank);
  seL4_MessageInfo_t response=seL4_Call(procs[STORAGE].ep,seL4_MessageInfo_new(0,0,0,14));
  if (seL4_MessageInfo_get_length(response)!=14 || seL4_GetMR(0)!=0) return false;
  volume_bank = seL4_GetMR(2);
  if (volume_bank < 1 || volume_bank > 2) return false;
  memcpy(local_catalog,new_catalog,length+16);
  source = 3;
  package_trial_installed();
  return true;
#else
  if (!volume_commit(new_catalog, length + 16, volume_bank))
    return false;
  package_trial_installed();
  return true;
#endif
}
static void map_shared(seL4_CPtr frame, struct task *t, unsigned va, seL4_CapRights_t rights) {
  ensure_table(t, va);
  unsigned copy = next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, SEL4_DEPTH,
                        seL4_CapInitThreadCNode, frame, SEL4_DEPTH, seL4_AllRights));
  check(
      seL4_ARM_Page_Map(copy, t->pd, va, rights,
                        seL4_ARM_Default_VMAttributes | seL4_ARM_ExecuteNever));
}
static void map_lane(seL4_CPtr frame, struct task *t, unsigned va) {
  map_shared(frame, t, va, seL4_AllRights);
}
static void workers(void) {
  static const unsigned slots[] = {2, 4, 3, 5, 6, 8};
  for (unsigned lane = TOUCH; lane <= PACKAGES; lane++) {
    unsigned index = lane - TOUCH, id = FIRST_WORKER + index,
             va = LANE_BASE + index * PAGE;
    if (!procs[lane].package)
      continue;
    struct process *w = &procs[id];
    w->package = procs[RUNTIME].package;
    prepare_process(id);
    unsigned frame = create(ram, seL4_ARM_SmallPageObject);
    map_lane(frame, &bootstrap, va);
    map_lane(frame, &w->task, va);
    map_lane(frame, &procs[RUNTIME].task, va);
    if (lane == RADIO && procs[POWER].package)
      grant(&w->task, 6, procs[POWER].ep, RADIO, false);
    if (lane == POWER)
      grant(&w->task, 4, procs[DISPLAY].ep, RUNTIME, false);
    grant(&w->task, slots[index], procs[lane].ep, RUNTIME, false);
    unsigned wake = create(ram, seL4_NotificationObject);
    grant(&w->task, 14, wake, 0, true);
    grant(&procs[RUNTIME].task, 9 + index, wake, 0, false);
  }
}
static void share_with_open_attrs(seL4_CPtr frame, unsigned va, seL4_ARM_VMAttributes attrs) {
  struct task *t = &procs[OPEN_RT].task;
  if (!t->pd) return;
  ensure_table(t, va);
  unsigned copy = next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, SEL4_DEPTH, seL4_CapInitThreadCNode, frame, SEL4_DEPTH,
                        seL4_AllRights));
  check(seL4_ARM_Page_Map(copy, t->pd, va, seL4_AllRights, attrs));
}
static void share_with_open(seL4_CPtr frame, unsigned va) {
  share_with_open_attrs(frame, va, seL4_ARM_ExecuteNever);
}
// Scanout, cached for the Open Runtime: it copies whole frames there and
// cleans the data cache over the rows it wrote (uncached, every store went to
// memory on its own, about 27 MB/s on the handset).
static void share_scanout_with_open(seL4_CPtr frame, unsigned va) {
  share_with_open_attrs(frame, va, seL4_ARM_PageCacheable | seL4_ARM_ExecuteNever);
}
// Kernel memory for the Open Runtime from any RAM untyped except the one the
// other processes share, so its app objects never crowd them out; with
// `shared`, the shared untyped last.
static seL4_CPtr spare_ram_in(unsigned type, unsigned bits, bool shared) {
  for (unsigned i = 0; i < boot->untyped.end - boot->untyped.start; i++) {
    seL4_CPtr u = boot->untyped.start + i;
    if (boot->untypedList[i].isDevice || u == ram || boot->untypedList[i].sizeBits < bits) continue;
    if (next_slot >= boot->empty.end) stop();
    if (!seL4_Untyped_Retype(u, type, bits, seL4_CapInitThreadCNode, 0, 0, next_slot, 1))
      return next_slot++;
  }
  // Last, the shared untyped, but never at the cost of booting: 0 means none.
  if (shared && !seL4_Untyped_Retype(ram, type, bits, seL4_CapInitThreadCNode, 0, 0, next_slot, 1))
    return next_slot++;
  return 0;
}
static seL4_CPtr spare_ram(unsigned type, unsigned bits) {
  return spare_ram_in(type, bits, true);
}
#ifdef VIOLET_MOTO
#if VM_AARCH64
// 1 MiB is no frame size on AArch64: a section is 256 pages in consecutive
// slots, all made from one 1 MiB untyped.
enum { SECTION_PAGES = 256 };
static seL4_CPtr section_pages(seL4_CPtr untyped) {
  if (boot->empty.end - next_slot < SECTION_PAGES) stop();
  seL4_CPtr first = next_slot;
  check(seL4_Untyped_Retype(untyped, seL4_ARM_SmallPageObject, 0, seL4_CapInitThreadCNode, 0, 0, next_slot, SECTION_PAGES));
  next_slot += SECTION_PAGES;
  return first;
}
// Map a section's pages at `va`: copies of them when `copy` (the originals stay where they are).
static void map_section(seL4_CPtr first, struct task *t, seL4_Word va, seL4_CapRights_t rights,
                        seL4_ARM_VMAttributes attrs, bool copy) {
  for (unsigned k = 0; k < SECTION_PAGES; k++) {
    seL4_CPtr frame = first + k;
    if (copy) {
      frame = next_slot++;
      check(seL4_CNode_Copy(seL4_CapInitThreadCNode, frame, SEL4_DEPTH, seL4_CapInitThreadCNode, first + k, SEL4_DEPTH,
                            seL4_AllRights));
    }
    // Never executable, whatever the caller asks (runtime/open/tests/test_page_rights.py).
    check(seL4_ARM_Page_Map(frame, t->pd, va + (seL4_Word)k * PAGE, rights, attrs | seL4_ARM_ExecuteNever));
  }
}
#endif
// A 1 MiB section from a spare untyped of exactly that size first (several
// are left around the reserved windows), so neither the shared untyped (8 MiB
// on the handset, which the supervisor allocates from to the end) nor a larger
// spare one the Open Runtime asks for is cut into.
static seL4_CPtr spare_section(void) {
#if VM_AARCH64
  for (unsigned i = 0; i < boot->untyped.end - boot->untyped.start; i++) {
    seL4_CPtr u = boot->untyped.start + i;
    if (boot->untypedList[i].isDevice || u == ram || boot->untypedList[i].sizeBits != 20) continue;
    if (boot->empty.end - next_slot < SECTION_PAGES) stop();
    if (!seL4_Untyped_Retype(u, seL4_ARM_SmallPageObject, 0, seL4_CapInitThreadCNode, 0, 0, next_slot, SECTION_PAGES)) {
      seL4_CPtr first = next_slot;
      next_slot += SECTION_PAGES;
      return first;
    }
  }
  seL4_CPtr untyped = spare_ram(seL4_UntypedObject, 20);
  return section_pages(untyped ? untyped : allocate(ram, seL4_UntypedObject, 20));
#else
  for (unsigned i = 0; i < boot->untyped.end - boot->untyped.start; i++) {
    seL4_CPtr u = boot->untyped.start + i;
    if (boot->untypedList[i].isDevice || u == ram || boot->untypedList[i].sizeBits != 20) continue;
    if (next_slot >= boot->empty.end) stop();
    if (!seL4_Untyped_Retype(u, seL4_ARM_SectionObject, 0, seL4_CapInitThreadCNode, 0, 0, next_slot, 1))
      return next_slot++;
  }
  seL4_CPtr section = spare_ram(seL4_ARM_SectionObject, 20);
  return section ? section : create(ram, seL4_ARM_SectionObject);
#endif
}
#endif
// How far Open Runtime setup got, and the root CNode's use, for the boot harness.
volatile unsigned open_setup[4], open_fault[6];
#if VM_AARCH64
volatile unsigned last_fault[6];
#endif
static void give_open(unsigned slot, seL4_CPtr cap) {
  check(seL4_CNode_Copy(procs[OPEN_RT].task.caps, slot, OPEN_BITS, seL4_CapInitThreadCNode, cap, SEL4_DEPTH,
                        seL4_AllRights));
}
// The Open Runtime builds app processes itself: it gets its own CSpace and
// VSpace, the ASID pool, kernel memory for app objects, a pool of physical
// memory for app pages and, in QEMU, the read-only /apps image.
static void open_runtime(void) {
  struct task *t = &procs[OPEN_RT].task;
  open_setup[0] = 1;
  open_setup[2] = next_slot;
  open_setup[3] = boot->empty.end;
  if (!procs[OPEN_RT].package) return;
  open_setup[0] = 2;
  if (t->cap_bits != OPEN_BITS) return;
  give_open(OPEN_SLOT_CNODE, t->caps);
  give_open(OPEN_SLOT_PD, t->pd);
  give_open(OPEN_SLOT_ASID, seL4_CapInitThreadASIDPool);
  // Its own thread, allowed to give app threads the priority every process
  // runs at and to raise itself above it while it waits for events.
  check(seL4_TCB_SetMCPriority(t->thread, seL4_CapInitThreadTCB, OPEN_EVENT_PRIORITY));
  give_open(OPEN_SLOT_AUTHORITY, t->thread);
#ifdef CONFIG_KERNEL_MCS
  // To give app threads scheduling contexts and move them between tiers:
  // one SchedControl per core (core 0 at OPEN_SLOT_SCHED_CONTROL, cores 1-7
  // from OPEN_SLOT_SCHED_CORES: slots 20-26), which also decides the core a thread runs on.
  give_open(OPEN_SLOT_SCHED_CONTROL, boot->schedcontrol.start);
  for (unsigned core = 1; core < boot->schedcontrol.end - boot->schedcontrol.start && core < 8; core++)
    give_open(OPEN_SLOT_SCHED_CORES + core - 1, boot->schedcontrol.start + core);
#endif
  // Its alarm clock: the nonsecure physical timer (PPI 30), which the kernel
  // lets user mode program, signals a notification bound to its thread. It
  // blocks between app calls and timer expiries instead of polling, and checks
  // at start that the timer fires (docs/8ms-frame-pipeline-rfc.md).
  seL4_CPtr wake = create(ram, seL4_NotificationObject);
  seL4_CPtr badged = next_slot++, handler = next_slot++;
  check(seL4_CNode_Mint(seL4_CapInitThreadCNode, badged, SEL4_DEPTH, seL4_CapInitThreadCNode, wake, SEL4_DEPTH,
                        seL4_AllRights, OPEN_TIMER_BADGE));
  // With the timekeeper running, it owns the timer and signals `badged`.
  // A kernel that keeps the timer from user level, or gives it only to
  // flagged threads (vm.h VM_TIMER_EVERYONE), gives it no timer: it polls.
  if (timekeeper_open(t, wake, badged)) {
  } else
  if (VM_TIMER_EVERYONE && !seL4_IRQControl_Get(seL4_CapIRQControl, OPEN_TIMER_IRQ, seL4_CapInitThreadCNode, handler, SEL4_DEPTH)) {
    open_timer_handler = handler;
    if (!seL4_IRQHandler_SetNotification(handler, badged) && !seL4_TCB_BindNotification(t->thread, wake)) {
      give_open(OPEN_SLOT_TIMER, handler);
      give_open(OPEN_SLOT_WAKE, wake);
    }
  }
#ifdef VIOLET_MOTO
  // Touch interrupts (services/touch/events.c): the controller's EINT 10
  // reaches GIC interrupt 203, bound to Touch's thread. Touch reads each
  // report as it arrives into a ring the Open Runtime maps read-only at
  // 0x09807000, then signals the Open Runtime's notification. The mark in
  // word 0 tells Touch its capabilities are in place.
  if (procs[TOUCH].package) {
    unsigned ring = create(ram, seL4_ARM_SmallPageObject);
    map_shared(ring, &procs[TOUCH].task, 0x09007000, seL4_AllRights);
    map_shared(ring, t, 0x09807000, seL4_CanRead);
    map_shared(ring, &bootstrap, 0x0970a000, seL4_AllRights);
    seL4_CPtr notification = create(ram, seL4_NotificationObject);
    seL4_CPtr badged_touch = next_slot++, irq = next_slot++;
    check(seL4_CNode_Mint(seL4_CapInitThreadCNode, badged_touch, SEL4_DEPTH, seL4_CapInitThreadCNode,
                          notification, SEL4_DEPTH, seL4_AllRights, TOUCH_IRQ_BADGE));
    if (!seL4_IRQControl_GetTrigger(seL4_CapIRQControl, TOUCH_GIC_IRQ, 0, seL4_CapInitThreadCNode, irq, SEL4_DEPTH)) {
      touch_irq_handler = irq;
      if (!seL4_IRQHandler_SetNotification(irq, badged_touch) &&
          !seL4_TCB_BindNotification(procs[TOUCH].task.thread, notification)) {
        check(seL4_CNode_Copy(procs[TOUCH].task.caps, TOUCH_SLOT_IRQ, CAP_BITS, seL4_CapInitThreadCNode,
                              irq, SEL4_DEPTH, seL4_AllRights));
        grant(&procs[TOUCH].task, TOUCH_SLOT_OPEN, wake, OPEN_TOUCH_BADGE, false);
        // Its own thread: it rises to the Open Runtime's priority once its
        // firmware is loaded, so a report is read as soon as it arrives.
        check(seL4_TCB_SetMCPriority(procs[TOUCH].task.thread, seL4_CapInitThreadTCB, OPEN_EVENT_PRIORITY));
        check(seL4_CNode_Copy(procs[TOUCH].task.caps, TOUCH_SLOT_TCB, CAP_BITS, seL4_CapInitThreadCNode,
                              procs[TOUCH].task.thread, SEL4_DEPTH, seL4_AllRights));
        ((volatile unsigned *)0x0970a000)[0] = 0x31515249; // "IRQ1"
      }
    }
  }
#endif
#ifdef VIOLET_QEMU
  seL4_CPtr kernel_memory = spare_ram(seL4_UntypedObject, OPEN_KERNEL_BITS);
  if (!kernel_memory) kernel_memory = spare_ram(seL4_UntypedObject, OPEN_KERNEL_BITS - 1);
#else
  // The handset: 8 MiB only from a spare untyped, never out of the shared one
  // the supervisor keeps allocating from (frames, restarts); otherwise the
  // 4 MiB it always had, spare first, then shared.
  seL4_CPtr kernel_memory = spare_ram_in(seL4_UntypedObject, OPEN_KERNEL_BITS, false);
  if (!kernel_memory) kernel_memory = spare_ram(seL4_UntypedObject, OPEN_KERNEL_BITS - 1);
#endif
  if (kernel_memory) give_open(OPEN_SLOT_KERNEL, kernel_memory);
#ifdef VIOLET_QEMU
  // The image QEMU loads at 0x4a000000, read-only at 0x10000000, then the pool.
  mapping = t;
  for (unsigned off = 0; off < 0x06000000; off += 0x01000000)
    map_device_rights(0x4a000000 + off, 0x10000000 + off, 24, seL4_CanRead);
  // 128 MiB at 0x50000000 (the block ends at a hole at 0x5f000000) and 256 MiB
  // at 0x60000000, below the framebuffer. The Open Runtime cuts them into pages.
  give_open(OPEN_SLOT_POOL, device_untyped(0x50000000, 27));
  give_open(OPEN_SLOT_POOL + 1, device_untyped(0x60000000, 28));
#else
  // The loader leaves "OPEN" at 0x43ffffec after staging the image at
  // 0xc0000000, when no live reservation touches 0xc0000000-0xdfffffff.
  open_setup[0] = 3;
#ifdef VIOLET_MOTO
  // The loader's verdict and image size, for USB.
  ((volatile unsigned *)0x09202000)[48] = ((volatile unsigned *)0x09f00000)[0xfffec / 4];
  ((volatile unsigned *)0x09202000)[49] = ((volatile unsigned *)0x09f00000)[0xfffe8 / 4];
  ((volatile unsigned *)0x09202000)[50] = ((volatile unsigned *)0x09f00000)[0xfffe4 / 4];
  ((volatile unsigned *)0x09202000)[51] = ((volatile unsigned *)0x09f00000)[0xfffe0 / 4];
  ((volatile unsigned *)0x09202000)[52] = ((volatile unsigned *)0x09f00000)[0xfffdc / 4];
  ((volatile unsigned *)0x09202000)[53] = ((volatile unsigned *)0x09f00000)[0xfffd8 / 4];
  // The AArch64 loader's "EL" word: the level LK entered at (word 54, USB request 0x59).
  ((volatile unsigned *)0x09202000)[54] = VM_AARCH64 ? ((volatile unsigned *)0x09f00000)[0xfffd0 / 4] : 0;
  // And why it could not open the boot history, if it could not (words 61-64).
  for (unsigned i = 0; i < 4; i++)
    ((volatile unsigned *)0x09202000)[61 + i] = VM_AARCH64 ? ((volatile unsigned *)0x09f00000)[0xfffc0 / 4 + i] : 0;
  // The AArch64 loader's verified-boot word: "VB" and the version it accepted (word 65).
  ((volatile unsigned *)0x09202000)[65] = VM_AARCH64 ? ((volatile unsigned *)0x09f00000)[0xfffbc / 4] : 0;
  // And its "SL" word: the boot slot LK started (word 66; mark_running_slot).
  ((volatile unsigned *)0x09202000)[66] = VM_AARCH64 ? ((volatile unsigned *)0x09f00000)[0xfffb8 / 4] : 0;
#endif
  if (((volatile unsigned *)0x09f00000)[0xfffec / 4] == 0x4e45504f) {
    open_setup[0] = 4;
    mapping = t;
    for (unsigned off = 0; off < 0x06000000; off += 0x01000000)
      map_device_rights(0xc0000000 + off, 0x10000000 + off, 24, seL4_CanRead);
    open_setup[0] = 5;
    give_open(OPEN_SLOT_POOL, device_untyped(0xd0000000, 28));
    open_setup[0] = 6;
  }
  // Open apps' sockets ("NET2", runtime/open/net.rs): a control page, 32 slot
  // headers and 24 KiB of rings a slot, 0xd0000 bytes served by Radio at
  // 0x0b000000 and used by the Open Runtime at 0x09400000. In 64 KiB frames,
  // but 4 KiB pages after the Bluetooth pages, which sit at +0x40000 in both,
  // and in the first 64 KiB: the control page is filled here. Its word 5 says
  // how much is mapped, so a Radio package never serves a slot whose rings a
  // supervisor (an older one says nothing) did not map.
  static const unsigned net_mapped[6] = {0, 0, 0, 0, 0, 0xd0000};
  for (unsigned off = 0; off < 0xd0000;) {
    if (off == 0x40000) off = 0x43000;
    // (AArch64 has no 64 KiB frame, its large page is 2 MiB: pages throughout.)
    unsigned large = !VM_AARCH64 && off >= 0x10000 && !(off & 0xffff);
    unsigned frame = create(ram, large ? seL4_ARM_LargePageObject : seL4_ARM_SmallPageObject);
    if (!off) fill_frame(frame, (const unsigned char *)net_mapped, sizeof net_mapped);
    map_shared(frame, &procs[RADIO].task, 0x0b000000 + off, seL4_AllRights);
    map_shared(frame, t, 0x09400000 + off, seL4_AllRights);
    off += large ? 0x10000 : PAGE;
  }
  // Bluetooth HCI for open apps (runtime/open/hci.rs): a control page with the
  // command ring and an event ring, served by Radio at 0x0b040000 (bt.rs) and
  // used by the Open Runtime at 0x09440000. A third page, Radio's alone, holds
  // the STP frame being received.
  for (unsigned i = 0; i < 3; i++) {
    unsigned frame = create(ram, seL4_ARM_SmallPageObject);
    map_shared(frame, &procs[RADIO].task, 0x0b040000 + i * PAGE, seL4_AllRights);
    if (i < 2) map_shared(frame, t, 0x09440000 + i * PAGE, seL4_AllRights);
  }
  // Installs over USB: a control page and a 64 KiB data window, written by USB
  // Diagnostics at 0x09210000 and appended to /apps by the Open Runtime at 0x09500000.
  for (unsigned i = 0; i < 17; i++) {
    unsigned frame = create(ram, seL4_ARM_SmallPageObject);
    map_shared(frame, &procs[USB_DIAG].task, 0x09210000 + i * PAGE, seL4_AllRights);
    map_shared(frame, t, 0x09500000 + i * PAGE, seL4_AllRights);
  }
  // USB Diagnostics reads the Open Runtime's log and passes launch requests.
  unsigned log_frame = create(ram, seL4_ARM_SmallPageObject);
  map_shared(log_frame, t, 0x09000000, seL4_AllRights);
  map_shared(log_frame, &procs[USB_DIAG].task, 0x09205000, seL4_CanRead);
  unsigned request_frame = create(ram, seL4_ARM_SmallPageObject);
  map_shared(request_frame, &procs[USB_DIAG].task, 0x09206000, seL4_AllRights);
  map_shared(request_frame, t, 0x09001000, seL4_CanRead);
#endif
  // Installed apps (runtime/open/appstore.rs): a 64 KiB window through which
  // Storage reads and writes the app extent, at 0x0d300000 in Storage and
  // 0x09130000 in the Open Runtime.
  if (procs[STORAGE].task.pd)
    for (unsigned i = 0; i < 16; i++) {
      unsigned frame = create(ram, seL4_ARM_SmallPageObject);
      map_shared(frame, &procs[STORAGE].task, 0x0d300000 + i * PAGE, seL4_AllRights);
      map_shared(frame, t, 0x09130000 + i * PAGE, seL4_AllRights);
    }
  grant(t, OPEN_SLOT_RUNTIME, procs[RUNTIME].ep, OPEN_RUNTIME_BADGE, false);
  open_setup[0] = 9;
  open_setup[1] = next_slot;
#ifdef VIOLET_MOTO
  for (unsigned i = 0; i < 2; i++) ((volatile unsigned *)0x09202000)[46 + i] = open_setup[i];
#endif
}
// Camera's frame memory, 4 MiB at 0x0b000000 in Camera and, read-only, at
// 0x0b000000 in the Open Runtime. On the handset it is physical 0xe0000000,
// which CAMSV writes; the loader leaves "CAMM" at 0x43ffffd4 when no
// reservation touches it. Both map it cacheable: Camera holds the (mapped)
// frame capabilities in its slots from 8 and invalidates each slot's lines
// once CAMSV has written a frame there, before publishing it. On AArch32 those
// are four 1 MiB sections (slots 8-11); on AArch64, which has no 1 MiB frame,
// two 2 MiB frames (slots 8-9): both addresses are 2 MiB aligned
// (targets/moto-g-pure/camera.rs FRAME_BITS). In QEMU Camera draws its frames
// itself, into guest RAM at 0x78000000.
// (The windows as the AArch32 board files describe them are the literal
// 1 MiB calls below, which targets/board checks; on AArch64 each even
// section's call maps that section and the next as one frame.)
enum { CAMERA_FRAME_SLOT = 8, CAMERA_FRAME_BITS = VM_AARCH64 ? 21 : 20 };
static void camera_frames(void) {
  if (!procs[CAMERA].package) return;
  seL4_ARM_VMAttributes attrs = seL4_ARM_ExecuteNever;
  for (unsigned off = 0; off < 0x400000; off += 0x100000) {
    seL4_CPtr frame;
    if (off & ((1u << CAMERA_FRAME_BITS) - 1)) continue;
#if VM_AARCH64
    seL4_Word frame_va = 0x0b000000 + off;
#endif
#ifdef VIOLET_MOTO
    if (((volatile unsigned *)0x09f00000)[0xfffd4 / 4] != 0x4d4d4143) return;
#if VM_AARCH64
    seL4_Word frame_pa = 0xe0000000u + off;
    device(CAMERA, frame_pa, frame_va, CAMERA_FRAME_BITS);
#else
    device(CAMERA, 0xe0000000 + off, 0x0b000000 + off, 20);
#endif
    // One frame either way (a section, or a 2 MiB frame on AArch64).
    if (WINDOW_COUNT != 1) stop();
    frame = WINDOW_FIRST;
    attrs = seL4_ARM_PageCacheable | seL4_ARM_ExecuteNever;
    check(seL4_ARM_Page_Unmap(frame));
    check(seL4_ARM_Page_Map(frame, procs[CAMERA].task.pd, 0x0b000000 + off, seL4_AllRights, attrs));
#else
    // Guest RAM the kernel does not manage, after the open-app pool (and
    // below the framebuffer), so the 64 MiB the kernel has stay for objects.
#if VM_AARCH64
    seL4_Word frame_pa = 0x78000000u + off;
    device(CAMERA, frame_pa, frame_va, CAMERA_FRAME_BITS);
#else
    device(CAMERA, 0x78000000 + off, 0x0b000000 + off, 20);
#endif
    frame = WINDOW_FIRST;
#endif
    struct task *t = &procs[OPEN_RT].task;
#if VM_AARCH64
    // The window's frames (WINDOW_COUNT of WINDOW_STEP bytes).
    for (unsigned k = 0; t->pd && k < WINDOW_COUNT; k++) {
      unsigned copy = next_slot++;
      check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, SEL4_DEPTH, seL4_CapInitThreadCNode, frame + k, SEL4_DEPTH,
                            seL4_AllRights));
      check(seL4_ARM_Page_Map(copy, t->pd, 0x0b000000 + off + k * WINDOW_STEP, seL4_CanRead, attrs));
    }
#else
    if (t->pd) {
      unsigned copy = next_slot++;
      check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, SEL4_DEPTH, seL4_CapInitThreadCNode, frame, SEL4_DEPTH,
                            seL4_AllRights));
      check(seL4_ARM_Page_Map(copy, t->pd, 0x0b000000 + off, seL4_CanRead, attrs));
    }
#endif
#ifdef VIOLET_MOTO
    check(seL4_CNode_Move(procs[CAMERA].task.caps, CAMERA_FRAME_SLOT + (off >> CAMERA_FRAME_BITS), CAP_BITS,
                          seL4_CapInitThreadCNode, frame, SEL4_DEPTH));
#endif
  }
}
static void wire(void) {
  build_group();
#ifdef CONFIG_KERNEL_MCS
  root_reply = create(ram, seL4_ReplyObject);
#endif
#ifdef VIOLET_QEMU
  // Device untypeds must be consumed in ascending physical address order.
  for (unsigned i = 0; i < sizeof(devices) / sizeof(*devices); i++) {
    unsigned bits = devices[i].bits ? devices[i].bits : 12;
    // A 64 KiB frame waits unmapped (made at one spare address here) for its process.
    device(0, devices[i].pa,
           bits == 16 ? 0x0c0f0000 : devices[i].id ? 0x0c000000 + i * PAGE : devices[i].va, bits);
#if VM_AARCH64
    devices[i].frame = WINDOW_FIRST;
    devices[i].pieces = WINDOW_COUNT;
    devices[i].piece_bits = window_bits;
    if (bits == 16)
      for (unsigned k = 0; k < devices[i].pieces; k++) check(seL4_ARM_Page_Unmap(devices[i].frame + k));
#else
    devices[i].frame = next_slot - 1;
    if (bits == 16) check(seL4_ARM_Page_Unmap(devices[i].frame));
#endif
  }
  report = device(0, 0x49300000, 0x09100000, 12);
  const struct archive *staged = device(0, 0x49400000, 0x09400000, 20);
  const struct archive *previous = device(0, 0x49500000, 0x09500000, 20);
  inbox = device(0, 0x49600000, 0x09600000, 20);
  local_catalog = device(0, 0x49700000, 0x09700000, 20);
  new_catalog = device(0, 0x49800000, 0x09800000, 20);
  if (!generation || source == 3)
    source = staged->magic ? 1 : 0;
  if (source == 1 && !parse(staged, 1 << 20))
    source = 2;
  if (source == 2 && !parse(previous, 1 << 20))
    source = 0;
  volume_bank = volume_load(local_catalog);
  if (volume_bank && parse(local_catalog, 1 << 20))
    source = 3;
#else
  report = handset_report;
  factory_usb = procs[USB_DIAG].package;
  check(!factory_usb);
  // 0x43c00000-0x43e00000: the installed catalog the loader read from its
  // bank (up to CATALOG_LIMIT), then the USB trial's scratch, then the
  // loader's bank word in the last word.
  local_catalog = device(0,0x43c00000,0x0d000000,20);
  device(0,0x43d00000,0x0d100000,20);
  usb_scratch = (unsigned char *)local_catalog + CATALOG_LIMIT;
  // The largest upload (a SERVICE_WINDOW image with its header and signature).
  _Static_assert(0x200000 - 4 - CATALOG_LIMIT >= 24 + SERVICE_WINDOW + 80, "the USB trial scratch is too small");
  // The catalog being composed: 2 MiB of RAM, made again each generation as
  // the inbox is, from spare untypeds (spare_section).
  for (unsigned i = 0; i < 2; i++) {
    catalog_candidate_frame[i] = spare_section();
#if VM_AARCH64
    map_section(catalog_candidate_frame[i], &bootstrap, 0x0d400000 + i * 0x100000, seL4_AllRights,
                seL4_ARM_Default_VMAttributes|seL4_ARM_ExecuteNever, false);
#else
    check(seL4_ARM_Page_Map(catalog_candidate_frame[i], bootstrap.pd, 0x0d400000 + i * 0x100000, seL4_AllRights,
                            seL4_ARM_Default_VMAttributes|seL4_ARM_ExecuteNever));
#endif
  }
  new_catalog = (void *)0x0d400000;
  if (!generation) volume_bank = *(volatile unsigned *)0x0d1ffffc;
  source = volume_bank>=1 && volume_bank<=2 && parse(local_catalog,CATALOG_LIMIT) ? 3 : 0;
  if (!source) volume_bank=0;
#if VM_AARCH64
  map_section(section_pages(allocate(ram, seL4_UntypedObject, 20)), &bootstrap, 0x0d200000, seL4_AllRights,
              seL4_ARM_Default_VMAttributes|seL4_ARM_ExecuteNever, false);
#else
  unsigned upload_frame = create(ram,seL4_ARM_SectionObject);
  check(seL4_ARM_Page_Map(upload_frame,bootstrap.pd,0x0d200000,seL4_AllRights,
                          seL4_ARM_Default_VMAttributes|seL4_ARM_ExecuteNever));
#endif
  inbox = (const void *)0x0d200000;
  upload_size = uploaded = 0;
  package_authorized = signature_started = false;
#endif
  // The install floors of the catalog that starts now: its packages and, for
  // an installed catalog, its floor record. They replace the last catalog's,
  // and become permanent (until power-off) once this one has run healthy.
  package_floor_begin();
  for (unsigned id = 1; id < TOTAL; id++)
    if ((id < COUNT || id == USB_DIAG || id == AUDIO || id == OPEN_RT || id == CAMERA || id == SENTINEL) &&
        id != PACKAGES && procs[id].package)
      package_floor_ran(id, package_version(procs[id].package));
  if (source && catalog_floors)
    package_floor_load(catalog_floors, catalog_floor_bytes);
  report[3] = source;
  __sync_synchronize();
  report[2] = ++generation;
  procs[PACKAGES].package = &recovery_manager;
  life = create(ram, seL4_EndpointObject);
  for (unsigned id = 1; id < COUNT; id++) {
    if (!procs[id].package)
      continue;
    prepare_process(id);
    if (id >= RUNTIME && id <= PACKAGES) {
      procs[id].ep = create(ram, seL4_EndpointObject);
      grant(&procs[id].task, id == STORAGE ? 2 : 1, procs[id].ep, 0, true);
    }
  }
  // Before Audio, which shares its PCM ring with the Open Runtime.
  prepare_process(OPEN_RT);
  if (procs[SENTINEL].package) {
    // Only the Open Runtime calls the Sentinel, and the Sentinel calls nobody.
    prepare_process(SENTINEL);
    procs[SENTINEL].ep = create(ram, seL4_EndpointObject);
    grant(&procs[SENTINEL].task, 1, procs[SENTINEL].ep, 0, true);
    // It runs only while the Open Runtime waits for its answer, and must not
    // wait behind app threads (120) for the CPU: the Open Runtime's own priority.
    check(seL4_TCB_SetPriority(procs[SENTINEL].task.thread, seL4_CapInitThreadTCB, OPEN_EVENT_PRIORITY));
    if (procs[OPEN_RT].task.pd) {
      grant(&procs[OPEN_RT].task, OPEN_SLOT_SENTINEL, procs[SENTINEL].ep, OPEN_RUNTIME_BADGE, false);
      for (unsigned i = 0; i < SENTINEL_WINDOW_PAGES; i++) {
        unsigned request = create(ram, seL4_ARM_SmallPageObject);
        map_shared(request, &procs[OPEN_RT].task, OPEN_SENTINEL_REQUEST + i * PAGE, seL4_AllRights);
        map_shared(request, &procs[SENTINEL].task, SENTINEL_REQUEST + i * PAGE, seL4_CanRead);
        unsigned reply = create(ram, seL4_ARM_SmallPageObject);
        map_shared(reply, &procs[SENTINEL].task, SENTINEL_REPLY + i * PAGE, seL4_AllRights);
        map_shared(reply, &procs[OPEN_RT].task, OPEN_SENTINEL_REPLY + i * PAGE, seL4_CanRead);
      }
    }
  }
  if (procs[AUDIO].package) {
    // Only Runtime calls Audio. On the handset Audio reaches the PMIC through Power.
    prepare_process(AUDIO);
    procs[AUDIO].ep = create(ram, seL4_EndpointObject);
    grant(&procs[AUDIO].task, 1, procs[AUDIO].ep, 0, true);
    grant(&procs[RUNTIME].task, RUNTIME_AUDIO_SLOT, procs[AUDIO].ep, RUNTIME, false);
    if (procs[POWER].package)
      grant(&procs[AUDIO].task, AUDIO_POWER_SLOT, procs[POWER].ep, AUDIO, false);
    // Raw PCM from open apps: a header page and a 16-page ring, at 0x09300000
    // in Audio and 0x09100000 in the Open Runtime, then an 8-page ring of
    // microphone samples for /dev/violet/mic.
    for (unsigned i = 0; i < 25; i++) {
      unsigned frame = create(ram, seL4_ARM_SmallPageObject);
      map_shared(frame, &procs[AUDIO].task, 0x09300000 + i * PAGE, seL4_AllRights);
      if (procs[OPEN_RT].task.pd)
        map_shared(frame, &procs[OPEN_RT].task, 0x09100000 + i * PAGE, seL4_AllRights);
    }
    // Audio's published state (services/audio/process.rs, Audio::snapshot):
    // Runtime reads it instead of calling Audio, which answers only between
    // hardware polls.
    unsigned state_frame = create(ram, seL4_ARM_SmallPageObject);
    map_shared(state_frame, &procs[AUDIO].task, 0x09320000, seL4_AllRights);
    map_shared(state_frame, &procs[RUNTIME].task, 0x07500000, seL4_CanRead);
  }
  if (procs[CAMERA].package) {
    // Only the Open Runtime talks to Camera, through one shared page
    // (services/camera/process.rs): its requests and Camera's state. The
    // frames follow, once the board's frame memory is known (camera_frames).
    prepare_process(CAMERA);
    if (procs[POWER].package)
      grant(&procs[CAMERA].task, CAMERA_POWER_SLOT, procs[POWER].ep, CAMERA, false);
    unsigned state_frame = create(ram, seL4_ARM_SmallPageObject);
    map_shared(state_frame, &procs[CAMERA].task, 0x09320000, seL4_AllRights);
    if (procs[OPEN_RT].task.pd)
      map_shared(state_frame, &procs[OPEN_RT].task, 0x09120000, seL4_AllRights);
  }
  for (unsigned id = 1; id < COUNT; id++)
    if ((id == LAUNCHER || id == WATCH || id >= WIFI) && procs[id].package)
      grant(&procs[id].task, 1, procs[RUNTIME].ep, id, false);
  grant(&procs[PACKAGES].task, 7, life, PACKAGES, false);
  grant(&procs[RUNTIME].task, 7, life, RUNTIME, false);
#ifdef VIOLET_QEMU
  for (unsigned i = 0; i < sizeof(devices) / sizeof(*devices); i++) {
    struct device_page d = devices[i];
    struct task *t = &procs[d.id].task;
    if (!t->pd)
      continue;
    // The root key blob (crypt cluster) is read-only to Storage, so a
    // stray write there cannot destroy the only copy of the key.
    seL4_CapRights_t rights = d.pa == 0x49003000 ? seL4_CanRead : seL4_AllRights;
#if VM_AARCH64
    for (unsigned k = 0; k < d.pieces; k++) {
      check(seL4_ARM_Page_Unmap(d.frame + k));
      check(seL4_ARM_Page_Map(d.frame + k, t->pd, d.va + ((seL4_Word)k << d.piece_bits), rights,
                              seL4_ARM_ExecuteNever));
    }
#else
    check(seL4_ARM_Page_Unmap(d.frame));
    ensure_table(t, d.va);
    check(seL4_ARM_Page_Map(d.frame, t->pd, d.va, rights,
                            seL4_ARM_ExecuteNever));
#endif
  }
  // The Sentinel derives its sealing key from the device root key
  // (system/sentinel/seal.rs `master`): the same blob page, read-only, with
  // Storage's attributes. Never the Open Runtime's.
  for (unsigned i = 0; i < sizeof(devices) / sizeof(*devices); i++)
    if (devices[i].pa == 0x49003000 && procs[SENTINEL].task.pd) {
      unsigned copy = next_slot++;
      check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, SEL4_DEPTH, seL4_CapInitThreadCNode,
                            devices[i].frame, SEL4_DEPTH, seL4_CanRead));
      ensure_table(&procs[SENTINEL].task, SENTINEL_ROOT_KEY);
      check(seL4_ARM_Page_Map(copy, procs[SENTINEL].task.pd, SENTINEL_ROOT_KEY, seL4_CanRead,
                              seL4_ARM_ExecuteNever));
    }
#else
  boot_stage(1);
  // Cacheable, as the supervisor maps the same frames while it composes the
  // catalog there. Storage once read them uncached: it saw RAM the
  // supervisor's dirty cache lines had not reached yet, and lines written
  // back while it worked changed the data under it, so the bank's checksum
  // (taken at the end) did not match the sectors written before, and the
  // loader refused the installed catalog at the next boot.
  for (unsigned i = 0; i < 2; i++) {
#if VM_AARCH64
    map_section(catalog_candidate_frame[i], &procs[STORAGE].task, 0x0d100000 + i * 0x100000, seL4_CanRead,
                seL4_ARM_Default_VMAttributes | seL4_ARM_ExecuteNever, true);
#else
    unsigned copy_catalog=next_slot++;
    check(seL4_CNode_Copy(seL4_CapInitThreadCNode,copy_catalog,SEL4_DEPTH,seL4_CapInitThreadCNode,
                          catalog_candidate_frame[i],SEL4_DEPTH,seL4_CanRead));
    check(seL4_ARM_Page_Map(copy_catalog,procs[STORAGE].task.pd,0x0d100000 + i * 0x100000,seL4_CanRead,
                            seL4_ARM_Default_VMAttributes | seL4_ARM_ExecuteNever));
#endif
  }
  if (usb_factory_only) procs[USB_DIAG].package = factory_usb;
  if (usb_trial_bytes) {
    // The scratch section survives group teardown. The inbox does not.
    memcpy((void *)inbox, usb_scratch, usb_trial_bytes);
    uploaded = upload_size = usb_trial_bytes;
    if (inbox->id == USB_DIAG && valid_inbox()) {
      procs[USB_DIAG].package = inbox;
      usb_trial_generation = generation;
      usb_deadline = ticks64() + (unsigned long long)ticks_per_ms * usb_trial_timeout_ms;
    } else {
      usb_trial_bytes = 0;
    }
  }
  prepare_process(USB_DIAG);
  grant(&procs[USB_DIAG].task,7,life,USB_TRANSFER_BADGE,false);
  grant(&procs[USB_DIAG].task,6,procs[POWER].ep,USB_DIAG,false);
  if (procs[AUDIO].package) {
    // Audio publishes diagnostics to USB; USB passes developer requests back.
    unsigned status_frame = create(ram, seL4_ARM_SmallPageObject);
    map_shared(status_frame, &procs[AUDIO].task, 0x09203000, seL4_AllRights);
    map_shared(status_frame, &procs[USB_DIAG].task, 0x09203000, seL4_CanRead);
    unsigned request_frame = create(ram, seL4_ARM_SmallPageObject);
    map_shared(request_frame, &procs[USB_DIAG].task, 0x09204000, seL4_AllRights);
    map_shared(request_frame, &procs[AUDIO].task, 0x09204000, seL4_CanRead);
    grant(&procs[AUDIO].task, 7, life, AUDIO_LIFE_BADGE, false);
  }
  if (procs[CAMERA].package) {
    // Camera's diagnostics and developer requests, as for Audio.
    unsigned status_frame = create(ram, seL4_ARM_SmallPageObject);
    map_shared(status_frame, &procs[CAMERA].task, 0x09203000, seL4_AllRights);
    map_shared(status_frame, &procs[USB_DIAG].task, 0x09207000, seL4_CanRead);
    unsigned request_frame = create(ram, seL4_ARM_SmallPageObject);
    map_shared(request_frame, &procs[USB_DIAG].task, 0x09208000, seL4_AllRights);
    map_shared(request_frame, &procs[CAMERA].task, 0x09204000, seL4_CanRead);
    // A 64 KiB preview Camera writes for `moto-package camera-shot`.
    for (unsigned i = 0; i < 16; i++) {
      unsigned frame = create(ram, seL4_ARM_SmallPageObject);
      map_shared(frame, &procs[CAMERA].task, 0x09340000 + i * PAGE, seL4_AllRights);
      map_shared(frame, &procs[USB_DIAG].task, 0x09230000 + i * PAGE, seL4_CanRead);
    }
    grant(&procs[CAMERA].task, 7, life, CAMERA_LIFE_BADGE, false);
  } else {
    // No Camera in this image: USB Diagnostics still reads these addresses
    // for camera-status and camera-shot, and a fault there suspends it for
    // the boot. Blank pages answer zeros and take requests nobody reads.
    unsigned blank = create(ram, seL4_ARM_SmallPageObject);
    map_shared(blank, &procs[USB_DIAG].task, 0x09207000, seL4_CanRead);
    for (unsigned i = 0; i < 16; i++) map_shared(blank, &procs[USB_DIAG].task, 0x09230000 + i * PAGE, seL4_CanRead);
    map_shared(create(ram, seL4_ARM_SmallPageObject), &procs[USB_DIAG].task, 0x09208000, seL4_AllRights);
  }
  unsigned diagnostic_frame = create(ram, seL4_ARM_SmallPageObject);
  map_lane(diagnostic_frame, &bootstrap, 0x09202000);
  map_shared(diagnostic_frame, &procs[USB_DIAG].task, 0x09202000, seL4_CanRead);
  for (unsigned id = RADIO; id <= POWER; id++) {
    unsigned frame = create(ram, seL4_ARM_SmallPageObject);
    unsigned va = 0x09200000 + (id - RADIO) * PAGE;
    map_shared(frame, &procs[id].task, va, seL4_AllRights);
    map_shared(frame, &procs[USB_DIAG].task, va, seL4_CanRead);
  }
  board_device(TOUCH, 0x10000000, 0x09000000, 0x09708000); // topckgen: audio clock muxes
  board_device(TOUCH, 0x10001000, 0x09001000, 0x09701000);
  device(TOUCH, 0x10002000, 0x09004000, 12);
  board_device(TOUCH, 0x10005000, 0x09002000, 0x09702000);
  device(0, 0x10006000, 0x09700000, 12);
  unsigned spm_frame = LAST_FRAME;
  ensure_table(&procs[RADIO].task, 0x09000000);
  unsigned spm_copy = next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, spm_copy, SEL4_DEPTH, seL4_CapInitThreadCNode,
                        spm_frame, SEL4_DEPTH, seL4_CanRead));
  check(seL4_ARM_Page_Map(spm_copy, procs[RADIO].task.pd, 0x09000000,
                          seL4_CanRead, seL4_ARM_ExecuteNever));
  grant(&procs[RADIO].task, 7, life, RADIO, false);
  // Radio's own thread, allowed up to 160, for its WMT handshake (transport.c, hold_cpu).
  check(seL4_TCB_SetMCPriority(procs[RADIO].task.thread, seL4_CapInitThreadTCB, 160));
  check(seL4_CNode_Copy(procs[RADIO].task.caps, 12, CAP_BITS, seL4_CapInitThreadCNode,
                        procs[RADIO].task.thread, SEL4_DEPTH, seL4_AllRights));
  board_device(TOUCH, 0x10007000, 0x09005000, 0x09703000);
  device(TOUCH, 0x1000b000, 0x09006000, 12); // EINT: the touch controller's interrupt line
  device(0, 0x1000c000, 0x0970d000, 12); // APMIXED: the MIPI receivers' 26 MHz gates
  device(POWER, 0x1000d000, 0x09000000, 12);
  if (procs[AUDIO].package) {
    // Keypad (Volume Down), read-only: Audio cannot reconfigure the scan.
    mapping = &procs[AUDIO].task;
    map_device_rights(0x10010000, 0x09000000, 12, seL4_CanRead);
    map_device(0x11007000, 0x09004000, 12); // I2C0: FS1603 speaker amplifier
  }
  if (procs[CAMERA].package)
    device(CAMERA, 0x11009000, 0x09002000, 12); // I2C2: the rear camera's control bus
  device(RADIO, 0x1100c000, 0x09010000, 12);
  device(TOUCH, 0x11013000, 0x09003000, 12);
  device(USB_DIAG, 0x11200000, 0x09000000, 12);
  if (procs[AUDIO].package) {
    // AFE registers, then its 36 KiB SRAM (the playback buffer).
    mapping = &procs[AUDIO].task;
    for (unsigned off = 0; off < 0xa000; off += PAGE)
      map_device(0x11220000 + off, 0x09010000 + off, 12);
  }
  board_device(STORAGE, 0x11230000, 0x0a000000, 0x0a000000);
  record_boot_stage(4);
  if (procs[CAMERA].package) {
    // The MIPI CSI receivers' analog front ends (CSI0A/B, CSI1A/B, CSI2A/B).
    mapping = &procs[CAMERA].task;
    for (unsigned off = 0; off < 0x6000; off += PAGE)
      map_device(0x11c10000 + off, 0x09020000 + off, 12);
  }
  device(USB_DIAG, 0x11cc0000, 0x09001000, 12);
  if (procs[CAMERA].package) {
    // MMSYS config (the camera's bridge to the MM bus is one of its clock
    // gates) and SMI common (the camera power domain's bus clamp).
    device(0, 0x14000000, 0x0970f000, 12);
    device(0, 0x14002000, 0x0970e000, 12);
  }
  if (procs[OPEN_RT].package) {
    // DISP_RDMA0, read-only: the Open Runtime reads the panel's scan line
    // there to present without tearing (docs/8ms-frame-pipeline-rfc.md).
    mapping = &procs[OPEN_RT].task;
    map_device_rights(0x1400d000, 0x09804000, 12, seL4_CanRead);
  }
  board_device(RADIO, 0x18002000, 0x09015000, 0x09706000); // connectivity MCU and ROM mailbox
  device(RADIO, 0x18004000, 0x09011000, 12);
  device(RADIO,0x18006000,0x09014000,12); // WLAN DMA packet scheduler
  board_device(RADIO,0x18007000,0x09012000,0x09704000); // always-on connectivity clock check
  mapping = &bootstrap;
  map_device_rights(0x180b1000, 0x09705000, 12, seL4_CanRead);
  board_device(RADIO,0x180c1000,0x09013000,0x09707000); // shared oscillator control
  if (procs[CAMERA].package) {
    // The camera subsystem, powered on request (camera_platform): its clock
    // gates, SMI larb3 (the camera's memory port), the four sensor interfaces
    // with their muxes and test pattern generators, and CAMSV1, which writes
    // what a sensor interface receives straight to memory.
    mapping = &procs[CAMERA].task;
    map_device(0x1a000000, 0x09000000, 12);
    map_device(0x1a002000, 0x09001000, 12);
    for (unsigned off = 0; off < 0x8000; off += PAGE)
      map_device(0x1a040000 + off, 0x09010000 + off, 12);
    map_device(0x1a050000, 0x09018000, 12);
  }
  mapping = &procs[RADIO].task;
  for (unsigned off = 0; off < 0x300000; off += 0x100000)
    map_device(0x42400000 + off, 0x09c00000 + off, 20);
  for (unsigned off = 0; off < 0x400000; off += 0x100000)
    map_device(0x42000000 + off, 0x09800000 + off, 20);
  for (unsigned off = 0; off < 0x200000; off += 0x100000)
    map_device_rights(0x43e00000 + off, 0x09400000 + off, 20, seL4_CanRead);
  // The loader's handoff words sit in the last page of 0x43f00000-0x44000000,
  // read at 0x09ffffxx: the whole section on AArch32, its last page (the
  // last frame made) on AArch64, at the same address.
  enum { HANDOFF_VA = VM_AARCH64 ? 0x09fff000 : 0x09f00000 };
  unsigned handoff_frame=LAST_FRAME, policy_cap=next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode,policy_cap,SEL4_DEPTH,seL4_CapInitThreadCNode,
                        handoff_frame,SEL4_DEPTH,seL4_CanRead));
  check(seL4_ARM_Page_Map(policy_cap,bootstrap.pd,HANDOFF_VA,seL4_CanRead,seL4_ARM_ExecuteNever));
  // Touch reads the loader's panel word at 0x43fffff0 to pick its firmware.
  unsigned touch_boot=next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode,touch_boot,SEL4_DEPTH,seL4_CapInitThreadCNode,
                        policy_cap,SEL4_DEPTH,seL4_CanRead));
  check(seL4_ARM_Page_Map(touch_boot,procs[TOUCH].task.pd,HANDOFF_VA,seL4_CanRead,seL4_ARM_ExecuteNever));
#endif
  workers();
#ifdef VIOLET_MOTO
  boot_stage(2);
#endif
  surface(LAUNCHER);
  surface(WATCH);
  for (unsigned id = WIFI; id < COUNT; id++)
    if (procs[id].package)
      surface(id);
  timekeeper_start();
  open_runtime();
  camera_frames();
  unsigned first = 0;
#ifdef VIOLET_MOTO
  struct task *display = &procs[DISPLAY].task;
  ensure_table(display, 0x08000000);
  for (unsigned k = 0; k < 65536 / VM_64K_STEP; k++) {
    unsigned copy = next_slot++;
    check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, SEL4_DEPTH, seL4_CapInitThreadCNode,
                          boot_display_frame + k, SEL4_DEPTH, seL4_AllRights));
    check(seL4_ARM_Page_Map(copy, display->pd, 0x08000000 + k * VM_64K_STEP, seL4_AllRights, seL4_ARM_ExecuteNever));
    share_scanout_with_open(boot_display_frame + k, 0x08000000 + k * VM_64K_STEP);
  }
  first = 65536;
#endif
  for (unsigned off = first; off < 720 * 1600 * 4; off += 65536) {
    device(DISPLAY, 0x79450000 + off, 0x08000000 + off, 16);
    // The Open Runtime draws the app in front straight to scanout (on
    // AArch64 the 64 KiB is sixteen pages).
    for (unsigned k = 0; k < WINDOW_COUNT; k++)
      share_scanout_with_open(WINDOW_FIRST + k, 0x08000000 + off + k * WINDOW_STEP);
  }
  timekeeper_attach_all();
}
static void dismantle(void) {
  timekeeper_stop();
  for (unsigned i = 1; i < TOTAL; i++)
    if (procs[i].task.thread)
      check(seL4_TCB_Suspend(procs[i].task.thread));
  for (unsigned i = boot->untyped.start; i < boot->untyped.end; i++)
    check(seL4_CNode_Revoke(seL4_CapInitThreadCNode, i, SEL4_DEPTH));
  // The last handler capability: the interrupt is free for the next build.
  if (open_timer_handler) {
    seL4_CNode_Delete(seL4_CapInitThreadCNode, open_timer_handler, SEL4_DEPTH);
    open_timer_handler = 0;
  }
  if (touch_irq_handler) {
    seL4_CNode_Delete(seL4_CapInitThreadCNode, touch_irq_handler, SEL4_DEPTH);
    touch_irq_handler = 0;
  }
}
unsigned ticks_per_ms;
static unsigned long long ticks64(void) { return vm_counter(); }
unsigned ticks(void) { return (unsigned)ticks64(); }
// Fault containment for the Open Runtime (runtime/open/contain.rs). A fault
// in it (a panic, a bad pointer) starts its thread again at the package
// entry on a fresh stack, with its memory as the fault left it: the runtime
// sees it has run before, closes the app it was serving and carries on for
// the rest. Only a real fault message does this, never a stall, and only as
// often as open_recover.h allows; otherwise the thread is suspended as before.
#include "open_recover.h"
static struct open_warm open_warm;
static bool open_faulted;
static bool open_warm_restart(void) {
  if (!open_faulted ||
      !open_warm_allowed(&open_warm, ticks64(), (unsigned long long)OPEN_WARM_QUIET_MS * ticks_per_ms))
    return false;
  seL4_UserContext regs = {.pc = BASE, .sp = OPEN_STACK_TOP, .VM_ARG0 = OPEN_RT};
  return !seL4_TCB_WriteRegisters(procs[OPEN_RT].task.thread, 1, 0, sizeof(regs) / sizeof(seL4_Word), &regs);
}
static unsigned stalled(unsigned timeout) {
  unsigned now = ticks();
  for (unsigned lane = TOUCH; lane <= PACKAGES; lane++) {
    if (!procs[lane].package)
      continue;
    volatile unsigned *box = (void *)(uintptr_t)(LANE_BASE + (lane - TOUCH) * PAGE);
    unsigned since = box[1020];
    unsigned deadline=timeout;
#ifdef VIOLET_MOTO
    // Firmware start, scan, WPA2 and DHCP run on one worker with individual deadlines.
    if (lane==RADIO) deadline=ticks_per_ms*60000u;
#endif
    if (since && (int)(now - since) > (int)deadline)
      return lane;
  }
  volatile unsigned *health = (void *)LANE_BASE;
  if ((int)(now - health[1021]) > (int)timeout)
    return RUNTIME;
  unsigned focus = health[1022];
  // Runtime's client 9 is the Open Runtime.
  if (focus && (int)(now - health[1023]) > (int)timeout)
    return focus == OPEN_RUNTIME_BADGE ? OPEN_RT : focus;
  return 0;
}
#ifdef VIOLET_MOTO
// Connectivity MCU stays in reset during the identity transaction.
static unsigned conn_ack(volatile unsigned *spm) {
  return ((spm[0x180 / 4] >> 1) & 1) | (spm[0x184 / 4] & 2);
}
static void write_board(volatile unsigned *reg, unsigned value) {
  *reg = value;
  __asm__ volatile("dsb sy" ::: "memory");
}
static bool await_field(volatile unsigned *reg, unsigned mask,
                        unsigned expected) {
  unsigned start = ticks();
  do {
    if ((*reg & mask) == expected)
      return true;
  } while (ticks() - start < 10 * ticks_per_ms);
  return false;
}
struct conn_backup {
  unsigned map, peri, osc, rom, irq, ctrl, reset, pin1, pin2, rx, tx, btif_gate,
      spm_config;
  bool powered, live, remapped;
};
static struct conn_backup conn_saved;
static volatile unsigned wifi_memory_policy;
static bool wifi_memory_prepared;
static volatile unsigned wifi_memory_checks;
// The Ellis kernel extension (AArch32 and AArch64) installs its board-only cap in the reserved SMC
// slot. No child owns this cap. The kernel makes only the four fixed monitor
// calls of wifi/table.h; the window policy (wifi/windows.h) runs here.
static int ellis_smc(unsigned label, unsigned length, unsigned a, unsigned b, unsigned reply[4]) {
  seL4_SetMR(0, a);
  seL4_SetMR(1, b);
  seL4_MessageInfo_t info = seL4_Call(seL4_CapSMC, seL4_MessageInfo_new(label, 0, 0, length));
  if (seL4_MessageInfo_get_label(info) || seL4_MessageInfo_get_length(info) > 4) return -1;
  for (unsigned i = 0; i < seL4_MessageInfo_get_length(info); i++) reply[i] = seL4_GetMR(i);
  return seL4_MessageInfo_get_length(info);
}
#include "../../platform/sel4/board/wifi/user.h"
static unsigned wifi_memory_request(bool prepare, unsigned reader) {
  volatile unsigned *diag = (void *)0x09202000;
  volatile unsigned *spm = (void *)0x09700000;
  if (prepare) {
    unsigned state = (spm[0x180/4] | spm[0x184/4]) & ((1u<<1) | (1u<<30));
    if (conn_saved.powered || conn_saved.live || conn_saved.remapped || state) return 0x30;
  }
  unsigned profile = *(volatile unsigned *)0x09fffff8;
  if (prepare && profile != 1 && profile != 2) return 0x31;
  if (reader > WIFI_OFFSET) return 0x43;
  ellis_smc_refused = 0;
  unsigned policy = radio_windows_request(prepare, reader);
  // The kernel refused a call: this supervisor and the kernel disagree.
  if (ellis_smc_refused) return 0x43;
  for (unsigned i = 0; i < 8; ++i) diag[18+i] = radio_window_raw[i / 4][i % 4];
  diag[17] = 0x3155504d;
  diag[26] = ++wifi_memory_checks;
  return policy;
}
static unsigned conn_diagnostic[7];
static void restore_rom(void) {
  volatile unsigned *infra = (void *)0x09701000, *reset = (void *)0x09703018;
  volatile unsigned *mcu = (void *)0x09706000, *xo = (void *)0x09707204;
  write_board(reset, 0x88001000u | (*reset & 0x00ffffffu));
  write_board(mcu + 0x150 / 4, conn_saved.irq);
  write_board(xo, conn_saved.osc);
  write_board(mcu + 0x114 / 4, conn_saved.rom);
  if (conn_saved.btif_gate)
    write_board(infra + 0x80 / 4, 1u << 31);
}
static unsigned prepare_conn_rom(void) {
  volatile unsigned *infra = (void *)0x09701000;
  volatile unsigned *mcu = (void *)0x09706000, *xo = (void *)0x09707204;
  conn_diagnostic[5] = mcu[0];
  conn_diagnostic[6] = mcu[1];
  if ((conn_diagnostic[6] & 0xffff) != 0x8a00)
    return 12;
  conn_saved.map = infra[0x380 / 4];
  conn_saved.peri = infra[0x388 / 4];
  conn_saved.osc = *xo;
  conn_saved.rom = mcu[0x114 / 4];
  conn_saved.irq = mcu[0x150 / 4];
  conn_saved.btif_gate = infra[0x90 / 4] & (1u << 31);
  conn_saved.remapped = true;
  write_board(infra + 0x380 / 4, (conn_saved.map & ~0xfffu) | 0x42);
  write_board(infra + 0x388 / 4,
              (conn_saved.peri & 0xf000ffffu) | (0x10u << 16));
  if ((infra[0x380 / 4] & 0xfff) == 0x42 &&
      ((infra[0x388 / 4] >> 16) & 0xfff) == 0x10) {
    return 1;
  }
  return 13;
}
static unsigned radio_domain(unsigned *seen, unsigned *chip, unsigned *clock,
                           unsigned *mailbox, bool finish) {
  volatile unsigned *spm = (void *)0x09700000, *infra = (void *)0x09701000,
                    *gpio = (void *)0x09702000, *reset = (void *)0x09703018;
  volatile unsigned *ctrl = spm + 0x32c / 4;
  unsigned before = *ctrl, reset_before = *reset & 0x00ffffff;
  unsigned pin1 = gpio[0x3f0 / 4], pin2 = gpio[0x3e0 / 4];
  const unsigned rx = (1u << 13) | (1u << 14) | (1u << 16), tx = 1u << 18;
  unsigned rx_before = infra[0x220 / 4] & rx, tx_before = infra[0x250 / 4] & tx;
  *chip = 0;
  *clock = 0;
  *mailbox = 0;
  *seen = conn_ack(spm);
  unsigned status = 4, start;
  if (!finish) {
    conn_diagnostic[0] = before;
    conn_diagnostic[1] = spm[0];
    conn_diagnostic[2] = spm[0x180 / 4];
    conn_diagnostic[3] = spm[0x184 / 4];
    conn_diagnostic[4] = infra[0x228 / 4];
    conn_diagnostic[5] = 0;
    conn_diagnostic[6] = 0;
  }
  if (finish) {
    if (!conn_saved.powered)
      return 2;
    if (conn_saved.remapped) restore_rom();
    conn_saved.live = false;
    before = conn_saved.ctrl;
    reset_before = conn_saved.reset;
    pin1 = conn_saved.pin1;
    pin2 = conn_saved.pin2;
    rx_before = conn_saved.rx;
    tx_before = conn_saved.tx;
    status = 1;
    goto isolate;
  }
  if (conn_saved.powered || conn_saved.live || conn_saved.remapped)
    return 2;
  if (!wifi_memory_prepared)
    return 15;
  conn_saved.ctrl = before;
  conn_saved.reset = reset_before;
  conn_saved.pin1 = pin1;
  conn_saved.pin2 = pin2;
  conn_saved.rx = rx_before;
  conn_saved.tx = tx_before;
  if ((before & 31) != 0x12 || *seen)
    return 2;
  conn_saved.spm_config = spm[0] & 0xffff;
  if (!(conn_saved.spm_config & 1)) {
    write_board(spm, 0x0b160001u | conn_saved.spm_config);
    if (!(spm[0] & 1))
      return 3;
  }
  write_board(gpio + 0x3f0 / 4, (pin1 & 0xffffff88) | 0x11);
  write_board(gpio + 0x3e0 / 4, (pin2 & 0x8888888f) | 0x11111110);
  write_board(reset, 0x88001000 | reset_before);
  write_board(ctrl, before | 4);
  if (!await_field(spm + 0x180 / 4, 2, 2)) {
    write_board(ctrl,before);
    if (!await_field(spm + 0x180 / 4, 2, 0)) return 5;
    goto restore_config;
  }
  write_board(ctrl, before | 12);
  start = ticks();
  do {
    *seen = conn_ack(spm);
    if (*seen == 3)
      break;
  } while (ticks() - start < 10 * ticks_per_ms);
  if (*seen == 3) {
    write_board(ctrl, *ctrl & ~16u);
    write_board(ctrl, *ctrl & ~2u);
    write_board(ctrl, *ctrl | 1u);
    // Release the connectivity bus reset (bit 9) while the ROM CPU stays
    // held in reset (bit 12). The two resets gate different clock domains.
    write_board(reset,0x88001000u | ((*reset & 0x00ffffffu) & ~(1u<<9)));
    write_board(infra + 0x2a4 / 4, (1u << 14) | (1u << 16));
    write_board(infra + 0x2a4 / 4, 1u << 13);
    write_board(infra + 0x2ac / 4, tx);
    status = 7;
    if (await_field(infra + 0x228 / 4, rx, 0) &&
        await_field(infra + 0x250 / 4, tx, 0)) {
      conn_saved.powered = true;
      return 1;
    }
  isolate:
    write_board(infra + 0x2a0 / 4, rx_before & (1u << 13));
    write_board(infra + 0x2a8 / 4, tx_before);
    write_board(infra + 0x2a0 / 4, rx_before & ~(1u << 13));
    // Keep the domain powered and the MCU held in reset if isolation failed.
    if (!await_field(infra + 0x228 / 4, rx, rx_before) ||
        !await_field(infra + 0x250 / 4, tx, tx_before))
      return 10;
    write_board(ctrl, *ctrl | 2u);
    write_board(ctrl, *ctrl | 16u);
    write_board(ctrl, *ctrl & ~1u);
  }
  write_board(ctrl, before | 8);
  write_board(ctrl, before);
  start = ticks();
  while (conn_ack(spm))
    if (ticks() - start >= 10 * ticks_per_ms)
      return 5;
  // Keep the reserved DMA mapping until the domain acknowledges power-off.
  if (conn_saved.remapped) {
    write_board(infra + 0x388 / 4, conn_saved.peri);
    write_board(infra + 0x380 / 4, conn_saved.map);
    conn_saved.remapped = false;
  }
restore_config:
  conn_saved.powered = false;
  write_board(reset, 0x88000000 | reset_before);
  write_board(gpio + 0x3e0 / 4,
              (gpio[0x3e0 / 4] & 0x8888888f) | (pin2 & ~0x8888888fu));
  write_board(gpio+0x3f0/4,(gpio[0x3f0/4]&0xffffff88)|(pin1&~0xffffff88u));
  if (!(conn_saved.spm_config&1)) write_board(spm,0x0b160000u|conn_saved.spm_config);
  return status;
}
#endif
#ifdef VIOLET_MOTO
// Audio clocks and pins, from Motorola's ellis kernel: mt6765 clk tables
// (clk-mt6765.c), AudDrv_Clk_On and the ellis pinctrl overlay. Bits in `want`:
// 1 audio clocks (infra audio + 26 MHz gates, audio_sel = 26 MHz,
// aud_intbus_sel = syspll1_d4), 2 headphone link pins (GPIO136/138/139 mode 1),
// 4 speaker pins (I2S3 on GPIO17-20 mode 6, I2C0 gate), 8 microphone link
// pins (AUD_CLK_MISO and AUD_DAT_MISO0/1 on GPIO140/142/143 mode 1). Zero leaves the
// hardware alone. The survey reports gates, muxes and pin modes either way.
static void set_field(volatile unsigned *reg, unsigned mask, unsigned value) {
  write_board(reg, (*reg & ~mask) | value);
}
static unsigned audio_platform(unsigned want, bool change, unsigned survey[10]) {
  volatile unsigned *top = (void *)0x09708000, *infra = (void *)0x09701000,
                    *gpio = (void *)0x09702000;
  unsigned done = 0;
  if (change && (want & 1)) {
    write_board(infra + 0x8c / 4, 1u << 25);   // ifr_audio
    write_board(infra + 0xa8 / 4, 1u << 4);    // ifr_audio26m
    write_board(top + 0x78 / 4, (3u << 24) | (1u << 31)); // audio_sel: clk26m, ungated
    write_board(top + 0x04 / 4, 1u << 15);
    write_board(top + 0x88 / 4, 3u | (1u << 7));         // aud_intbus_sel: clear, ungate
    write_board(top + 0x84 / 4, 1u);                     // syspll1_d4
    write_board(top + 0x04 / 4, 1u << 16);
    if (!(infra[0x94 / 4] & (1u << 25)) && !(infra[0xac / 4] & (1u << 4))) done |= 1;
  }
  if (change && (want & 2)) {
    set_field(gpio + 0x410 / 4, 0xff0f, 0x1101);
    done |= 2;
  }
  if (change && (want & 8)) {
    set_field(gpio + 0x410 / 4, 0xff0f0000u, 0x11010000u);
    done |= 8;
  }
  if (change && (want & 4)) {
    set_field(gpio + 0x320 / 4, 0xffff0, 0x66660);
    write_board(infra + 0x84 / 4, 1u << 11);   // ifr_i2c_ap
    if (!(infra[0x90 / 4] & (1u << 11))) done |= 4;
  }
  survey[0] = infra[0x90 / 4];
  survey[1] = infra[0x94 / 4];
  survey[2] = infra[0xac / 4];
  survey[3] = top[0x70 / 4];
  survey[4] = top[0x80 / 4];
  survey[5] = gpio[0x320 / 4];
  survey[6] = gpio[0x3a0 / 4];
  survey[7] = gpio[0x3b0 / 4];
  survey[8] = gpio[0x410 / 4];
  survey[9] = 0x41555031; // "AUP1"
  return done;
}
// Camera power, clocks and pins, from Motorola's ellis kernel: the CAM power
// domain (spm_mtcmos_ctrl_cam_pwr and _bus_prot in clk-mt6765-pg.c), camtg_sel
// at 24 MHz (usb20_192m_d8, clk-mt6765.c) and the ellis camera pinctrl
// (mt6762-ellis-camera-sensor-overlay.dtsi). The low byte of `val` names the
// items to change and the next byte their new state:
//   1 CAM power domain and seninf_sel   2 MCLK0: camtg 24 MHz, GPIO99 CMMCLK0
//   4 AVDD switch (GPIO92)   8 rear reset (GPIO101, high = running)
//   16 front reset (GPIO102) 32 I2C2 pins (GPIO103/104) and its clocks
//   64 MIPI CSI0 26 MHz gate 128 torch (GPIO154)
// The survey reports what the hardware shows either way.
static void gpio_out(volatile unsigned *gpio, unsigned pin, bool high) {
  unsigned bank = pin / 32 * 0x10 / 4, bit = 1u << (pin % 32);
  write_board(gpio + 0x300 / 4 + pin / 8 * 4 + 2, 0xfu << (pin % 8 * 4)); // mode 0 (GPIO)
  write_board(gpio + 0x100 / 4 + bank + (high ? 1 : 2), bit);              // DOUT set/clear
  write_board(gpio + 0x000 / 4 + bank + 1, bit);                           // DIR set: output
}
static void gpio_mode(volatile unsigned *gpio, unsigned pin, unsigned mode) {
  set_field(gpio + 0x300 / 4 + pin / 8 * 4, 0xfu << (pin % 8 * 4), mode << (pin % 8 * 4));
}
static unsigned camera_platform(unsigned val, bool change, unsigned survey[10]) {
  volatile unsigned *top = (void *)0x09708000, *infra = (void *)0x09701000,
                    *gpio = (void *)0x09702000, *spm = (void *)0x09700000,
                    *apmixed = (void *)0x0970d000, *smi = (void *)0x0970e000,
                    *mmsys = (void *)0x0970f000;
  volatile unsigned *cam = spm + 0x344 / 4;
  unsigned which = change ? val & 0xff : 0, on = val >> 8, done = 0;
  if (which & 1) {
    if (!(spm[0] & 1)) write_board(spm, 0x0b160001u | (spm[0] & 0xffff));
    if (on & 1) {
      write_board(cam, *cam | 4);   // PWR_ON
      write_board(cam, *cam | 8);   // PWR_ON_2ND
      bool up = await_field(spm + 0x180 / 4, 1u << 25, 1u << 25) &&
                await_field(spm + 0x184 / 4, 1u << 25, 1u << 25);
      write_board(cam, *cam & ~16u); // PWR_CLK_DIS
      write_board(cam, *cam & ~2u);  // PWR_ISO
      write_board(cam, *cam | 1);    // PWR_RST_B
      write_board(cam, *cam & ~(1u << 8));
      up = up && await_field(cam, 1u << 12, 0);
      write_board(cam, *cam & ~(1u << 9));
      up = up && await_field(cam, 1u << 13, 0);
      write_board(infra + 0x2a4 / 4, 1u << 20);              // bus protect step 2
      write_board(smi + 0x3c8 / 4, 1u << 3);                 // SMI clamp
      write_board(infra + 0x2ac / 4, (1u << 19) | (1u << 21)); // bus protect step 1
      write_board(mmsys + 0x108 / 4, 1u << 25); // mm_smi_cam_ck: SMI larb3's bridge (gals-cam2mm)
      // seninf_sel, the sensor interfaces' clock: univpll1_d4 (312 MHz), ungated.
      write_board(top + 0xa8 / 4, (3u << 16) | (1u << 23));
      write_board(top + 0xa4 / 4, 1u << 16);
      write_board(top + 0x04 / 4, 1u << 26);
      // camtm_sel, the receivers' timing clock: univpll2_d2 (208 MHz), ungated.
      write_board(top + 0xb8 / 4, (3u << 8) | (1u << 15));
      write_board(top + 0xb4 / 4, 3u << 8);
      write_board(top + 0x04 / 4, 1u << 29);
      if (up) done |= 1;
    } else {
      write_board(infra + 0x2a8 / 4, (1u << 19) | (1u << 21));
      bool idle = await_field(infra + 0x258 / 4, (1u << 19) | (1u << 21), (1u << 19) | (1u << 21));
      write_board(infra + 0x2a0 / 4, 1u << 20);
      idle = idle && await_field(infra + 0x228 / 4, 1u << 20, 1u << 20);
      write_board(smi + 0x3c4 / 4, 1u << 3);
      idle = idle && await_field(smi + 0x3c0 / 4, 1u << 3, 1u << 3);
      write_board(top + 0xa4 / 4, 1u << 23); // gate seninf_sel
      write_board(mmsys + 0x104 / 4, 1u << 25);
      write_board(top + 0xb4 / 4, 1u << 15); // and camtm_sel
      write_board(cam, *cam | (3u << 8));
      idle = idle && await_field(cam, 3u << 12, 3u << 12);
      write_board(cam, *cam | 2);
      write_board(cam, *cam | 16);
      write_board(cam, *cam & ~1u);
      write_board(cam, *cam & ~4u);
      write_board(cam, *cam & ~8u);
      idle = idle && await_field(spm + 0x180 / 4, 1u << 25, 0) &&
             await_field(spm + 0x184 / 4, 1u << 25, 0);
      if (idle) done |= 1;
    }
  }
  if (which & 2) {
    if (on & 2) {
      write_board(top + 0x58 / 4, (7u << 16) | (1u << 23)); // camtg_sel: clear, ungate
      write_board(top + 0x54 / 4, 1u << 16);                // usb20_192m_d8 = 24 MHz
      write_board(top + 0x04 / 4, 1u << 6);
      gpio_mode(gpio, 99, 1);
    } else {
      gpio_out(gpio, 99, false);
      write_board(top + 0x54 / 4, 1u << 23);
    }
    done |= 2;
  }
  if (which & 4) gpio_out(gpio, 92, on & 4), done |= 4;
  if (which & 8) gpio_out(gpio, 101, on & 8), done |= 8;
  if (which & 16) gpio_out(gpio, 102, on & 16), done |= 16;
  if (which & 32) {
    gpio_mode(gpio, 103, 1);
    gpio_mode(gpio, 104, 1);
    write_board(infra + 0x84 / 4, 1u << 11); // ifr_i2c_ap
    // I2C2 is a shared, arbitrated bus: its arbiter and IMM clocks
    // (ifr_i2c2a, ifr_i2c2_imm) must run too.
    write_board(infra + 0xa8 / 4, (1u << 23) | (1u << 24));
    if (!(infra[0x90 / 4] & (1u << 11)) && !(infra[0xac / 4] & (3u << 23))) done |= 32;
  }
  if (which & 64) {
    // mipic0_26m and mipic1_26m (clk-mt6765.c GATE_APMIXED 6 and 11)
    set_field(apmixed + 0x14 / 4, (1u << 6) | (1u << 11), (on & 64) ? (1u << 6) | (1u << 11) : 0);
    done |= 64;
  }
  if (which & 128) gpio_out(gpio, 154, on & 128), done |= 128;
  survey[0] = *cam;
  survey[1] = spm[0x180 / 4];
  survey[2] = infra[0xac / 4];  // infra gates: I2C2 arbiter and IMM
  survey[3] = (top[0x50 / 4] >> 16 & 0xff) | (top[0xa0 / 4] >> 16 & 0xff) << 8 |
              (top[0xb0 / 4] >> 8 & 0xff) << 16; // camtg_sel, seninf_sel, camtm_sel
  survey[4] = gpio[0x3b0 / 4];  // GPIO88-95 modes
  survey[5] = gpio[0x3c0 / 4];  // GPIO96-103 modes
  survey[6] = gpio[0x130 / 4];  // GPIO96-127 levels
  // GPIO96-111 input levels (SCL 103, SDA 104), and how often MCLK (99) changed in 256 reads.
  unsigned toggles = 0, last = gpio[0x230 / 4];
  for (unsigned i = 0; i < 256; i++) {
    unsigned now = gpio[0x230 / 4];
    toggles += ((now ^ last) >> 3) & 1;
    last = now;
  }
  survey[7] = (last & 0xffff) | toggles << 16;
  survey[8] = (apmixed[0x14 / 4] & 0xffff) | (mmsys[0x100 / 4] >> 25 & 1) << 16;
  survey[9] = infra[0x258 / 4] | (smi[0x3c0 / 4] & 8) << 28;
  return done;
}
#endif
// Full-system updates. Storage (services/storage/slotwrite.rs) writes the
// inactive boot slot for Settings or the Agent and hashes what it reads
// back; only the supervisor may make that slot the next one booted, after
// checking the manifest's signature, target and version and that Storage's
// digest is the signed one. "OTA1" checks the manifest alone (before the
// image is sent), "OTA2" checks everything and switches.
enum { OTA_CHECK = 0x3141544f, OTA_APPLY = 0x3241544f };
static bool ota_storage(unsigned op, unsigned value, const unsigned in[8], unsigned out[12]) {
  for (unsigned i = 0; i < 14; i++) seL4_SetMR(i, 0);
  seL4_SetMR(0, op);
  seL4_SetMR(2, value);
  for (unsigned i = 0; in && i < 8; i++) seL4_SetMR(4 + i, in[i]);
  seL4_MessageInfo_t reply = seL4_Call(procs[STORAGE].ep, seL4_MessageInfo_new(0, 0, 0, 14));
  if (seL4_MessageInfo_get_length(reply) != 14 || seL4_GetMR(0) != 0) return false;
  // out: value, arg, then the 40 bytes of data and tail.
  for (unsigned i = 0; out && i < 12; i++) out[i] = seL4_GetMR(2 + i);
  return true;
}
#if defined(VIOLET_MOTO) && VM_AARCH64
// The AArch64 handset: ask Storage to mark the boot slot the loader saw LK
// start (diagnostic word 66, "SL" and 1 or 2) successful, the stock HAL's
// MarkBootSuccessful on misc's bootloader_control (targets/moto-g-pure/emmc.rs
// mark_running_slot). For USB Diagnostics (request 0x5a, "VSL1"): word 67
// Storage's status (0 done), 68 the attempts, 69 whether it wrote, 70 the
// slot, 71-78 the record as read, 79-80 its slot halfwords afterwards.
static bool mark_running_slot(void) {
  volatile unsigned *diag = (void *)0x09202000;
  for (unsigned i = 0; i < 14; i++) seL4_SetMR(i, 0);
  seL4_SetMR(0, 0x4f54414b);
  seL4_SetMR(2, diag[66]);
  seL4_MessageInfo_t reply = seL4_Call(procs[STORAGE].ep, seL4_MessageInfo_new(0, 0, 0, 14));
  unsigned status = seL4_MessageInfo_get_length(reply) == 14 ? (unsigned)seL4_GetMR(0) : 0xffffffffu;
  diag[68] += 1;
  if (status == 0)
    for (unsigned i = 0; i < 12; i++) diag[69 + i] = (unsigned)seL4_GetMR(2 + i);
  __sync_synchronize();
  diag[67] = status;
  return status == 0;
}
#endif
// The update's version, or 0 with *why set (ota.rs; 8: Storage refused or
// has no inactive slot, 9: the switch itself failed).
static unsigned ota_request(bool apply, unsigned *why) {
  unsigned info[12], words[12], manifest[32], sectors = 0, digest[8];
  *why = 8;
  if (!procs[STORAGE].package || !ota_storage(0x4f544149, 0, 0, info) || info[0] < 2) return 0;
  for (unsigned at = 0; at < 128; at += 40) {
    if (!ota_storage(0x4f54414d, at, 0, words)) return 0;
    for (unsigned i = 0; i < 10 && at / 4 + i < 32; i++) manifest[at / 4 + i] = words[2 + i];
  }
  unsigned version = ota_verify((const unsigned char *)manifest, info[0], 0, &sectors, why);
  if (!version || !apply) return version;
  *why = 8;
  if (!ota_storage(0x4f544144, sectors, 0, words) || words[0] != sectors) return 0;
  for (unsigned i = 0; i < 8; i++) digest[i] = words[2 + i];
  version = ota_verify((const unsigned char *)manifest, info[0], (const unsigned char *)digest, &sectors, why);
  if (!version) return 0;
  *why = 9;
  return ota_storage(0x4f544153, sectors, digest, 0) ? version : 0;
}
void root_main(seL4_BootInfo *bi) {
  unsigned freq = vm_counter_frequency();
  ticks_per_ms = freq / 1000;
  unsigned faults = 0;
  boot = bi;
  seL4_SetIPCBuffer(bi->ipcBuffer);
  for (;;) {
    wire();
#ifdef VIOLET_MOTO
    record_boot_stage(5);
    ((volatile unsigned *)0x09202000)[16] = *(volatile unsigned *)0x09fffff4;
    wifi_memory_policy = 0;
    wifi_memory_prepared = false;
#endif
#ifdef VIOLET_TEST_ISOLATION
    // Execute a single load in the launcher's real CSpace and VSpace.
    // Its fault endpoint is the same one used for production supervision.
    extern const unsigned probe_image[];
    seL4_UserContext regs = {.pc = BASE, .sp = STACK_TOP, .VM_ARG0 = report[0]};
    fill_frame(procs[LAUNCHER].frames[0], (const unsigned char *)probe_image,
               8);
    check(seL4_TCB_WriteRegisters(procs[LAUNCHER].task.thread, 1, 0,
                                  sizeof(regs) / sizeof(seL4_Word), &regs));
#else
    for (unsigned i = RUNTIME; i < COUNT; i++)
      if (i <= PACKAGES && procs[i].package)
        start(i);
    for (unsigned w = FIRST_WORKER; w < TOTAL; w++)
      if (procs[w].package)
        start(w);
    start(LAUNCHER);
#ifdef VIOLET_MOTO
    boot_stage(3);
#endif
    volatile unsigned *health = (void *)LANE_BASE;
    unsigned healthy_since = ticks();
    bool settled = false;
    ota_mark_restart();
    health[1021] = healthy_since;
    unsigned limit = freq * (report[5] == 1 ? 2 : 20);
    check(seL4_TCB_SetPriority(seL4_CapInitThreadTCB, seL4_CapInitThreadTCB,
                               100));
#endif
    for (;;) {
      // A whole word: the kernel stores one there (64 bits on AArch64).
      seL4_Word badge = 0;
#ifdef VIOLET_TEST_ISOLATION
      seL4_MessageInfo_t msg = IPC_RECV(life, &badge, root_reply);
#else
      // Blocks until a message or the next health check when the timekeeper runs.
      seL4_MessageInfo_t msg = timekeeper_receive(life, &badge);
      open_faulted = badge == 16 + OPEN_RT; // a fault message, not the stall check below
#ifdef VIOLET_MOTO
      if (usb_trial_bytes && ticks64() >= usb_deadline) {
        usb_last_failure = USB_TIMEOUT;
        usb_trial_bytes = 0;
        break;
      }
#endif
      if (!badge) {
#ifdef VIOLET_MOTO
        volatile unsigned *diag = (void *)0x09202000;
        volatile unsigned *ready = (void *)(LANE_BASE + 0xf80);
        diag[0] = generation;
        diag[1] = health[1022];
        diag[2] = report[4];
        if (ready[0] && !diag[3]) {
          __sync_synchronize();
          for (unsigned i = 1; i < 12; i++) diag[i+3] = ready[i];
          __sync_synchronize();
          diag[3] = 1;
        }
#endif
        unsigned id = stalled(limit);
        if (id)
          badge = 16 + id;
        else {
          if (ticks() - healthy_since > limit) {
            faults = 0;
            // A healthy period ends the catalog's trial and keeps its floors.
            if (!settled) {
              settled = true;
              package_trial_healthy();
            }
            // And the boot slot: the bootloader stops spending its tries on
            // it (Storage writes nothing if it is already marked). A request
            // Storage could not carry out is sent again a little later.
#if defined(VIOLET_MOTO) && VM_AARCH64
            if (procs[STORAGE].package && ota_mark_due(settled, ticks()))
              ota_mark_result(mark_running_slot(), ticks(), freq);
#else
            if (procs[STORAGE].package && ota_mark_due(settled, ticks()))
              ota_mark_result(ota_storage(0x4f54414b, 0, 0, 0), ticks(), freq);
#endif
          }
          seL4_Yield();
          continue;
        }
      }
#endif
      // An Open Runtime fault stops open apps only: record it and suspend. A
      // just-installed catalog still rolls back, as for any other component.
      if (badge == 16 + OPEN_RT && source != 1) {
        open_fault[0] = 0x544c4146; // "FALT"
        open_fault[1] = seL4_MessageInfo_get_label(msg);
        for (unsigned i = 0; i < 4; i++) open_fault[2 + i] = seL4_GetMR(i);
#ifdef VIOLET_MOTO
        for (unsigned i = 0; i < 6; i++) ((volatile unsigned *)0x09202000)[40 + i] = open_fault[i];
#endif
        if (open_warm_restart()) continue; // contained: the runtime closes one app and goes on
        check(seL4_TCB_Suspend(procs[OPEN_RT].task.thread));
        continue;
      }
      if (badge >= 16) {
#if VM_AARCH64
        // The last fault: who, the fault's label and its first message words
        // (on AArch64 a VM fault is pc, address, prefetch, status).
        last_fault[0] = (unsigned)badge - 16;
        last_fault[1] = seL4_MessageInfo_get_label(msg);
        for (unsigned i = 0; i < 4; i++) last_fault[2 + i] = (unsigned)seL4_GetMR(i);
#ifdef VIOLET_MOTO
        // For USB Diagnostics (request 0x59, "VAR1"), words 55-60.
        for (unsigned i = 0; i < 6; i++) ((volatile unsigned *)0x09202000)[55 + i] = last_fault[i];
#endif
#endif
#ifdef VIOLET_TEST_ISOLATION
        if (seL4_MessageInfo_get_label(msg) == seL4_Fault_VMFault &&
            seL4_GetMR(1) == report[0])
          report[1] = report[0];
        stop();
#endif
#ifdef VIOLET_MOTO
        if (badge == 16 + USB_DIAG) {
          if (usb_trial_bytes) { usb_last_failure = USB_FAULT; usb_trial_bytes = 0; break; }
          if (procs[USB_DIAG].package != factory_usb) { usb_factory_only = true; break; }
          check(seL4_TCB_Suspend(procs[USB_DIAG].task.thread));
          continue;
        }
#endif
#ifdef VIOLET_MOTO
        if (usb_trial_bytes) { usb_last_failure = USB_DEPENDENCY_FAULT; usb_trial_bytes = 0; break; }
        // Only a catalog still on trial is thrown away. One that has run
        // healthy, or was found at power-on, restarts as it is: a crash must
        // not bring back the older packages of the other bank or the factory.
        if (source == 3 && package_trial_fault_rejects()) {
          // Freeze clients before taking the controller back from Storage.
          for (unsigned id=1; id<TOTAL; ++id)
            if (procs[id].task.thread) check(seL4_TCB_Suspend(procs[id].task.thread));
          volatile unsigned *msdc=(void*)0x0a000000;
          unsigned begin=ticks();
          while ((msdc[0x3c/4]&3) && ticks()-begin<100*ticks_per_ms) {}
          if (!(msdc[0x3c/4]&3) && volume_reject(volume_bank))
            volume_bank=volume_load(local_catalog);
          else volume_bank=0;
          source=volume_bank?3:0;
        }
#endif
        if (++faults >= 5)
          stop();
        unsigned failed = badge - 16;
        report[4] = failed >= FIRST_WORKER && failed < USB_DIAG ? RUNTIME : failed;
#ifdef VIOLET_QEMU
        // As on the handset: only an installed catalog still on trial is rejected.
        if (source == 3 && package_trial_fault_rejects() && !volume_reject(volume_bank))
          stop();
        if (source == 1)
          source = 2;
        else if (source == 2)
          source = 0;
#endif
        break;
      }
      unsigned op = seL4_GetMR(0), key = seL4_GetMR(1), val = seL4_GetMR(2),
               result = 0, error = 2;
#ifdef VIOLET_MOTO
      if (badge == AUDIO_LIFE_BADGE && key == AUDIO_PLATFORM &&
          seL4_MessageInfo_get_length(msg) == 14 && (op == GET || (op == EXECUTE && val <= 15))) {
        unsigned survey[10];
        result = audio_platform(op == EXECUTE ? val : 0, op == EXECUTE, survey);
        for (unsigned i = 0; i < 10; i++) seL4_SetMR(4 + i, survey[i]);
        seL4_SetMR(0, 0);
        seL4_SetMR(2, result);
        IPC_REPLY(root_reply, seL4_MessageInfo_new(0, 0, 0, 14));
        continue;
      }
      if (badge == CAMERA_LIFE_BADGE && key == CAMERA_PLATFORM &&
          seL4_MessageInfo_get_length(msg) == 14 && (op == GET || (op == EXECUTE && val <= 0xffff))) {
        unsigned survey[10];
        result = camera_platform(val, op == EXECUTE, survey);
        for (unsigned i = 0; i < 10; i++) seL4_SetMR(4 + i, survey[i]);
        seL4_SetMR(0, 0);
        seL4_SetMR(2, result);
        seL4_Reply(seL4_MessageInfo_new(0, 0, 0, 14));
        continue;
      }
      if (badge == USB_TRANSFER_BADGE && op == EXECUTE && seL4_MessageInfo_get_length(msg)==14) {
        unsigned n=seL4_GetMR(3), ok=0;
        if (key == 0x6d) {
          seL4_SetMR(0,0);
          seL4_SetMR(2,procs[USB_DIAG].package->crc);
          seL4_SetMR(3,usb_trial_bytes ? generation : 0);
          seL4_SetMR(4,usb_last_failure);
          IPC_REPLY(root_reply, seL4_MessageInfo_new(0,0,0,5));
          continue;
        } else if (key == 0x6e && usb_trial_bytes && val == usb_trial_generation) {
          if (update_catalog(USB_DIAG,false)) {
            usb_factory_only = false;
            usb_last_failure = USB_HEALTHY;
            usb_trial_bytes = 0;
            break;
          }
        } else if (usb_trial_bytes && key != 0x63) {
          // A candidate can report health; privileged controls resume after confirmation.
          ok = 0;
        } else if (key == 0x6a || key == 0x6b) {
          unsigned words[9];
          for (unsigned i = 0; i < 9; ++i) words[i] = seL4_GetMR(4+i);
          if (key == 0x6a && val && n && n <= 32) {
            memset(profile_key, 0, sizeof(profile_key));
            memcpy(profile_name, words, n);
            profile_length = n;
            profile_transaction = val;
            profile_ready = false;
            profile_pending = true;
            ok = 1;
          } else if (key == 0x6b && profile_pending && val == profile_transaction && n == 36 && words[8] >= 1704067200u) {
            memcpy(profile_key, words, 32);
            profile_epoch = words[8];
            profile_ticks = ticks64();
            profile_ready = true;
            profile_pending = false;
            ok = 1;
          }
          for (unsigned i = 0; i < 14; ++i) seL4_SetMR(i, 0);
          seL4_SetMR(0, ok ? 0 : 2);
          IPC_REPLY(root_reply, seL4_MessageInfo_new(0,0,0,1));
          continue;
        } else if (key == 0x66 || key == 0x67) {
          unsigned words[10];
          for (unsigned i=0; i<10; i++) words[i]=seL4_GetMR(4+i);
          package_authorized = false;
          if (key == 0x66) {
            signature_started = valid_inbox() && (inbox->id == RADIO || inbox->id == USB_DIAG) && n == 40;
            if (signature_started) memcpy(package_signature, words, 40);
            ok = signature_started;
          } else {
            if (signature_started && n && n <= sizeof(package_signature)-40) {
              memcpy(package_signature+40, words, n);
              package_authorized = signed_update(40+n);
            }
            signature_started = false;
            ok = package_authorized;
          }
        } else if (key == 0x68) {
          if (package_authorized) {
            package_authorized = false;
            if (!package_floor_installs(inbox->id, package_version(inbox))) {
              // Older than a version that has already run.
            } else if (inbox->id == USB_DIAG) {
              usb_trial_bytes = uploaded;
              memcpy(usb_scratch,inbox,usb_trial_bytes);
              break;
            }
            if (update_catalog(RADIO, false)) break;
          }
        } else if (key == 0x64) {
          // Read just the stopwatch checkpoint through the storage owner.
          seL4_SetMR(0, 0);
          seL4_SetMR(1, 0);
          seL4_MessageInfo_t reply = seL4_Call(procs[STORAGE].ep, seL4_MessageInfo_new(0,0,0,2));
          unsigned saved = seL4_GetMR(1);
          ok = seL4_MessageInfo_get_length(reply) == 2 && seL4_GetMR(0);
          seL4_SetMR(0, ok ? 0 : 3);
          seL4_SetMR(2, saved);
          IPC_REPLY(root_reply, seL4_MessageInfo_new(0,0,0,14));
          continue;
        } else if (key == 0x65) {
          // Stop new writes and serialize the final flush with Storage's outstanding work.
          for (unsigned id = 1; id < TOTAL; id++)
            if (id != STORAGE && procs[id].running) check(seL4_TCB_Suspend(procs[id].task.thread));
          seL4_SetMR(0, 0x56464c53);
          seL4_MessageInfo_t reply = seL4_Call(procs[STORAGE].ep, seL4_MessageInfo_new(0,0,0,14));
          if (seL4_MessageInfo_get_length(reply) == 14 && seL4_GetMR(0) == 0) {
            // A warm reset with the connectivity domain still powered (for example
            // while hosting the relay access point) can stop the next boot of slot B.
            if (conn_saved.powered) {
              unsigned seen = 0, chip = 0, clock = 0, mailbox = 0;
              radio_domain(&seen, &chip, &clock, &mailbox, true);
            }
            volatile unsigned *reset = (void *)0x09703014;
            for (;;) { *reset = 0x1209; __asm__ volatile("dsb sy" ::: "memory"); }
          }
          for (unsigned id = 1; id < TOTAL; id++)
            if (id != STORAGE && procs[id].running) check(seL4_TCB_Resume(procs[id].task.thread));
        } else if (key == 0x69 || (key==0x6c && val && profile_ready)) {
          join_job = key==0x6c ? val : 0;
          volatile unsigned *generation=(void *)(LANE_BASE+982*sizeof(unsigned));
          if (++*generation==0) ++*generation;
          ok = 1;
        } else if(key==0x63) {
          ((volatile unsigned *)LANE_BASE)[980]=val;
          ((volatile unsigned *)LANE_BASE)[981]=n;ok=1;
        } else if(key==0x60) { // New upload invalidates every previous partial package.
          package_authorized = signature_started = false;
          uploaded=upload_size=0;
          if(val>=sizeof(*inbox)+SIGNATURE && val<=sizeof(*inbox)+SERVICE_WINDOW+SIGNATURE) {
            upload_size=val;upload_crc=n;ok=1;
          }
        } else if(key==0x61 && n && n<=40 && val==uploaded && n<=upload_size-uploaded) {
          unsigned words[10];
          for(unsigned i=0;i<10;i++) words[i]=seL4_GetMR(4+i);
          memcpy((unsigned char *)inbox+uploaded,words,n);
          uploaded+=n;ok=1;
          if(uploaded==upload_size && (crc32((const unsigned char*)inbox,uploaded)!=upload_crc ||
              !valid_package(inbox,uploaded) || sizeof(*inbox)+inbox->size+SIGNATURE!=uploaded)) {
            upload_size=uploaded=0;ok=0;
          }
        } else if(key==0x62) { ok=1; }
        seL4_SetMR(0,ok?0:2);seL4_SetMR(1,upload_size);seL4_SetMR(2,uploaded);
        seL4_SetMR(3,procs[RADIO].package?procs[RADIO].package->crc:0);
        seL4_SetMR(4,(key==0x62 && valid_inbox())?inbox->id:0);seL4_SetMR(5,volume_bank);
        IPC_REPLY(root_reply, seL4_MessageInfo_new(0,0,0,14));continue;
      }
#endif
#ifdef VIOLET_MOTO
      if (badge==RADIO && op==GET && key==0x4060 && join_job && profile_ready &&
          seL4_MessageInfo_get_length(msg)==14) {
        unsigned words[8]={0};
        memcpy(words,profile_name,profile_length);
        seL4_SetMR(0,0);seL4_SetMR(2,profile_length);seL4_SetMR(3,join_job);
        for(unsigned i=0;i<8;i++) seL4_SetMR(4+i,words[i]);
        join_job=0;
        IPC_REPLY(root_reply, seL4_MessageInfo_new(0,0,0,14));continue;
      }
      if (badge == RADIO && op == GET && key == 0x405d &&
          seL4_MessageInfo_get_length(msg) == 14) {
        unsigned words[8];
        for (unsigned i=0;i<8;i++) words[i]=seL4_GetMR(4+i);
        bool ok=profile_ready && val == profile_length && !memcmp(words, profile_name, val);
        for (unsigned i=0;i<14;i++) seL4_SetMR(i,0);
        if (ok) {
          memcpy(words,profile_key,sizeof(words));
          for (unsigned i=0;i<8;i++) seL4_SetMR(4+i,words[i]);
        }
        seL4_SetMR(0,ok ? 0 : 3);
        IPC_REPLY(root_reply, seL4_MessageInfo_new(0,0,0,14));
        continue;
      }
#endif
      for (unsigned i = 0; i < 14; i++)
        seL4_SetMR(i, 0);
#ifdef VIOLET_MOTO
      if (badge == RADIO && key == 0x405e && op == GET && profile_ready &&
          seL4_MessageInfo_get_length(msg) == 14) {
        unsigned freq = vm_counter_frequency();
        if (freq) {
          unsigned long long elapsed=(ticks64()-profile_ticks)/freq;
          if (elapsed <= 0xffffffffu-profile_epoch) { result=profile_epoch+(unsigned)elapsed; error=0; }
        }
      } else if (badge == RADIO && key == 0x405c && op == GET &&
          seL4_MessageInfo_get_length(msg) == 14) {
        unsigned words[4];
        if (ellis_random(words)) {
          for (unsigned i=0;i<4;i++) seL4_SetMR(4+i,words[i]);
          result = 16;
          error = 0;
        }
      } else if (badge == RADIO && key == 0x4058 && val <= 2 && seL4_MessageInfo_get_length(msg) == 14 && (op == GET || op == EXECUTE)) {
        wifi_memory_policy = wifi_memory_request(op == EXECUTE, val);
        if (op == EXECUTE) wifi_memory_prepared = wifi_memory_policy == 1;
        result = wifi_memory_policy;
        error = 0;
      } else if (badge == RADIO && op == EXECUTE && (key == 0x4059 || key == 0x405a || key == 0x405b || key == 0x4054)) {
        unsigned seen = 0, chip = 0, clock = 0, mailbox = 0;
        if (key == 0x405b) {
          result = 2;
          if (conn_saved.powered && conn_saved.remapped && !conn_saved.live) {
            volatile unsigned *reset = (void *)0x09703018;
            // The shared bus gate and reset controller stay with the supervisor.
            write_board((void *)0x09701084, 1u << 31);
            write_board(reset, 0x88000000u | (*reset & 0x00ffefffu));
            conn_saved.live = true;
            result = 1;
          }
        } else if (key == 0x405a) {
          // Validate hardware before touching the gated ROM or chip-ID registers.
          result = 2;
          if (conn_saved.powered && !conn_saved.live && !conn_saved.remapped) {
            seen = conn_ack((void *)0x09700000);
            if (seen == 3) {
              clock = *(volatile unsigned *)0x09704000;
              result = 8;
              if ((clock & 0x70000) == 0x30000) {
                chip = *(volatile unsigned *)0x09705010;
                result = chip == 0x10020300 ? prepare_conn_rom() : 9;
              }
            }
          }
        } else {
          result = radio_domain(&seen,&chip,&clock,&mailbox,key == 0x4054);
        }
        seL4_SetMR(6,mailbox);
        for (unsigned i=0;i<7;i++) seL4_SetMR(7+i,conn_diagnostic[i]);
        seL4_SetMR(4,chip);
        seL4_SetMR(5,clock);
        seL4_SetMR(3, seen);
        error = 0;
      } else
#endif
      if (badge == PACKAGES && op == EXECUTE && key == 17 && (val == OTA_CHECK || val == OTA_APPLY)) {
        // A full-system update (ota_request): the version, or the refusal in its place.
        unsigned why = 0;
        result = ota_request(val == OTA_APPLY, &why);
        error = result ? 0 : 2;
        if (!result) result = why;
        for (unsigned i = 0; i < 14; i++) seL4_SetMR(i, 0);
        seL4_SetMR(3, ota_system_version());
      } else
      if (badge == PACKAGES && op == EXECUTE && (key == 17 || key == 18)) {
        if (val != USB_DIAG && update_catalog(val, key == 18))
          break;
      } else if (badge == PACKAGES && op == GET && key == 16) {
        error = 0;
        if (valid_inbox()) {
          result = inbox->id;
          reply_title(inbox);
        }
      } else if (badge == PACKAGES && op == GET && key == INVENTORY) {
        error = 0;
        for (unsigned id = 1; id < COUNT; id++)
          if (id != PACKAGES && procs[id].package)
            result |= 1u << id;
        if (procs[AUDIO].package)
          result |= 1u << AUDIO;
        if (procs[OPEN_RT].package)
          result |= 1u << OPEN_RT;
        if (procs[CAMERA].package)
          result |= 1u << CAMERA;
        if (procs[SENTINEL].package)
          result |= 1u << SENTINEL;
      } else if (badge == PACKAGES && op == GET && key == 9 && val < COUNT) {
        error = 0;
        if (procs[val].package && val != PACKAGES) {
          result = val;
          reply_title(procs[val].package);
        }
      } else if (badge == RUNTIME && seL4_MessageInfo_get_length(msg) == 14) {
        if (op == EXECUTE && key == OPEN &&
            (val == LAUNCHER || (val < COUNT && (val == WATCH || val >= WIFI) &&
                                 procs[val].package))) {
          start(val);
          error = 0;
        } else if (op == EXECUTE && key == QUIT && val < COUNT &&
                   (val == WATCH || val >= WIFI) && procs[val].package) {
          quit_app(val);
          error = 0;
        }
      }
      seL4_SetMR(0, error);
      seL4_SetMR(2, result);
      IPC_REPLY(root_reply, seL4_MessageInfo_new(0, 0, 0, 14));
    }

#ifdef VIOLET_MOTO
    if (conn_saved.powered) {
      unsigned seen,chip,clock,mailbox;
      radio_domain(&seen,&chip,&clock,&mailbox,true);
    }
#endif
    dismantle();
  }
}
#ifdef VIOLET_TEST_ISOLATION
#if VM_AARCH64
const unsigned probe_image[] = {0xf9400001, 0x14000000}; // ldr x1, [x0]; b .
#else
const unsigned probe_image[] = {0xe5900000, 0xeafffffe};
#endif
#endif
