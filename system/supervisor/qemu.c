// Initial task. Child images have independent code, data, stack and CSpaces.
// Only this process holds untyped memory, child TCBs and their address spaces.
#include "../../platform/sel4/board/memory.h"
#include "tasks.h"

seL4_IPCBuffer *__sel4_ipc_buffer;
char __sel4_print_error;
extern const unsigned char builtin_catalog[], builtin_catalog_end[];
extern const struct package recovery_manager;
extern void *memset(void *, int, unsigned);
extern void *memcpy(void *, const void *, unsigned);
enum {
  BASE = 0x100000,
  PAGE = 4096,
  SCRATCH = 0x03000000,
  CAP_BITS = 4,
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
  COUNT
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
static struct process procs[COUNT];
static seL4_CPtr life, scratch_slot;
static volatile unsigned *report;
static unsigned generation, source;
static unsigned crc32(const unsigned char *b, unsigned n) {
  unsigned v = ~0u;
  while (n--) {
    v ^= *b++;
    for (unsigned j = 0; j < 8; j++)
      v = (v >> 1) ^ ((0 - (v & 1)) & 0xedb88320);
  }
  return ~v;
}
static bool parse(const struct archive *a, unsigned limit) {
  if (a->magic != 0x31544356 || limit < 16 || a->size > limit - 16 ||
      a->count >= COUNT || crc32(a->bytes, a->size) != a->crc)
    return false;
  const struct package *found[COUNT] = {0};
  unsigned pos = 0;
  for (unsigned i = 0; i < a->count; i++) {
    if (a->size - pos < sizeof(struct package))
      return false;
    const struct package *p = (const void *)(a->bytes + pos);
    if (p->magic != 0x314b5056 || p->abi != 1 || p->id < 1 ||
        (p->id == PACKAGES || p->id >= COUNT) || found[p->id] || !p->code ||
        p->code % PAGE || p->size < p->code || p->size > 0x3e000 ||
        p->size > a->size - pos - sizeof(*p) ||
        crc32(p->bytes, p->size) != p->crc)
      return false;
    pos += (sizeof(*p) + p->size + 3) & ~3u;
    if (pos > a->size)
      return false;
    found[p->id] = p;
  }
  if (pos != a->size)
    return false;
  for (unsigned i = 1; i < COUNT; i++)
    if ((i == LAUNCHER || i == RUNTIME || i == TOUCH || i == DISPLAY) &&
        !found[i])
      return false;
  for (unsigned i = 1; i < COUNT; i++)
    procs[i].package = found[i];
  return true;
}
static void grant(struct task *t, unsigned slot, seL4_CPtr ep, unsigned badge,
                  bool server) {
  check(seL4_CNode_Mint(t->caps, slot, CAP_BITS, seL4_CapInitThreadCNode, ep,
                        32, seL4_CapRights_new(!server, 0, server, !server),
                        badge));
}
static void fill_frame(seL4_CPtr frame, const unsigned char *data, unsigned n) {
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, scratch_slot, 32,
                        seL4_CapInitThreadCNode, frame, 32, seL4_AllRights));
  check(seL4_ARM_Page_Map(scratch_slot, bootstrap.pd, SCRATCH, seL4_AllRights,
                          seL4_ARM_Default_VMAttributes));
  if (n)
    memcpy((void *)SCRATCH, data, n);
  memset((void *)(SCRATCH + n), 0, PAGE - n);
  check(seL4_ARM_Page_Unify_Instruction(scratch_slot, 0, PAGE));
  check(seL4_ARM_Page_Unmap(scratch_slot));
  check(seL4_CNode_Delete(seL4_CapInitThreadCNode, scratch_slot, 32));
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
  else
    p->frames[(va - BASE) / PAGE] = frame;
}
static void prepare_process(unsigned id) {
  struct process *p = &procs[id];
  const struct package *pkg = p->package;
  if (!pkg)
    return;
  struct task *t = &p->task;
  t->pd = create(ram, seL4_ARM_PageDirectoryObject);
  check(seL4_ARM_ASIDPool_Assign(seL4_CapInitThreadASIDPool, t->pd));
  t->caps = allocate(ram, seL4_CapTableObject, CAP_BITS);
  t->thread = create(ram, seL4_TCBObject);
  for (unsigned off = 0; off < pkg->size; off += PAGE) {
    unsigned n = pkg->size - off;
    private_page(p, BASE + off, pkg->bytes + off, n > PAGE ? PAGE : n,
                 off < pkg->code);
  }
  private_page(p, STACK_TOP - 2 * PAGE, 0, 0, false);
  private_page(p, STACK_TOP - PAGE, 0, 0, false);
  private_page(p, IPC_BUFFER, 0, 0, false);
  grant(t, FAULT_SLOT, life, 16 + id, false);
  unsigned guard = seL4_CNode_CapData_new(0, 32 - CAP_BITS).words[0];
  check(seL4_TCB_Configure(t->thread, FAULT_SLOT, t->caps, guard, t->pd, 0,
                           IPC_BUFFER, t->ipc));
  check(seL4_TCB_SetPriority(t->thread, seL4_CapInitThreadTCB, 100));
}
static void start(unsigned id) {
  struct process *p = &procs[id];
  if (p->running)
    return;
  seL4_UserContext regs = {.pc = BASE, .sp = STACK_TOP};
  check(seL4_TCB_WriteRegisters(p->task.thread, 1, 0,
                                sizeof(regs) / sizeof(unsigned), &regs));
  p->running = true;
}
static void quit_watch(void) {
  struct process *p = &procs[WATCH];
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
  for (unsigned off = 0; off < 240 * 400 * 4; off += 65536) {
    seL4_CPtr f = create(ram, seL4_ARM_LargePageObject);
    check(seL4_ARM_Page_Map(
        f, display->pd, 0x06000000 + (id - 1) * 0x100000 + off,
        seL4_CapRights_new(0, 0, 1, 0),
        seL4_ARM_Default_VMAttributes | seL4_ARM_ExecuteNever));
    if (app->pd) {
      seL4_CPtr copy = next_slot++;
      check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, 32,
                            seL4_CapInitThreadCNode, f, 32, seL4_AllRights));
      check(seL4_ARM_Page_Map(copy, app->pd, 0x06000000 + off, seL4_AllRights,
                              seL4_ARM_Default_VMAttributes |
                                  seL4_ARM_ExecuteNever));
    }
  }
}
static void *device(unsigned id, unsigned pa, unsigned va, unsigned bits) {
  mapping = id ? &procs[id].task : &bootstrap;
  return map_device(pa, va, bits);
}
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
static void build_group(void) {
  reset_allocator();
  const struct archive *factory = (const void *)builtin_catalog;
  check(!parse(factory, builtin_catalog_end - builtin_catalog));
}
struct device_page {
  unsigned id, pa, va;
  seL4_CPtr frame;
};
static struct device_page devices[] = {
    {TOUCH, 0x09000000, 0x09003000, 0},   {STORAGE, 0x0a000000, 0x0a000000, 0},
    {RADIO, 0x0a001000, 0x09001000, 0},   {TOUCH, 0x49000000, 0x09005000, 0},
    {RADIO, 0x49001000, 0x09000000, 0},   {POWER, 0x49002000, 0x09000000, 0},
    {STORAGE, 0x49200000, 0x0b000000, 0}, {RADIO, 0x49201000, 0x09002000, 0}};
static void wire(void) {
  build_group();
  // Device untypeds must be consumed in ascending physical address order.
  for (unsigned i = 0; i < sizeof(devices) / sizeof(*devices); i++) {
    device(0, devices[i].pa, 0x0c000000 + i * PAGE, 12);
    devices[i].frame = next_slot - 1;
  }
  report = device(0, 0x49300000, 0x09100000, 12);
  const struct archive *staged = device(0, 0x49400000, 0x09400000, 20);
  const struct archive *previous = device(0, 0x49500000, 0x09500000, 20);
  if (!generation)
    source = staged->magic ? 1 : 0;
  if (source == 1 && !parse(staged, 1 << 20))
    source = 2;
  if (source == 2 && !parse(previous, 1 << 20))
    source = 0;
  report[3] = source;
  __sync_synchronize();
  report[2] = ++generation;
  procs[PACKAGES].package = &recovery_manager;
  life = create(ram, seL4_EndpointObject);
  for (unsigned id = 1; id < COUNT; id++) {
    if (!procs[id].package)
      continue;
    prepare_process(id);
    if (id >= RUNTIME && id != WIFI) {
      procs[id].ep = create(ram, seL4_EndpointObject);
      grant(&procs[id].task, id == STORAGE ? 2 : 1, procs[id].ep, 0, true);
    }
  }
  for (unsigned id = 1; id < COUNT; id++)
    if ((id == LAUNCHER || id == WATCH || id == WIFI) && procs[id].package)
      grant(&procs[id].task, 1, procs[RUNTIME].ep, id, false);
  static const unsigned service_slots[COUNT] = {
      [TOUCH] = 2, [STORAGE] = 3, [DISPLAY] = 4, [RADIO] = 5, [POWER] = 6};
  for (unsigned id = TOUCH; id < PACKAGES; id++)
    if (procs[id].package)
      grant(&procs[RUNTIME].task, service_slots[id], procs[id].ep, RUNTIME,
            false);
  grant(&procs[RUNTIME].task, 8, procs[PACKAGES].ep, RUNTIME, false);
  grant(&procs[PACKAGES].task, 7, life, PACKAGES, false);
  grant(&procs[RUNTIME].task, 7, life, RUNTIME, false);
  for (unsigned i = 0; i < sizeof(devices) / sizeof(*devices); i++) {
    struct device_page d = devices[i];
    struct task *t = &procs[d.id].task;
    if (!t->pd)
      continue;
    check(seL4_ARM_Page_Unmap(d.frame));
    ensure_table(t, d.va);
    check(seL4_ARM_Page_Map(d.frame, t->pd, d.va, seL4_AllRights,
                            seL4_ARM_ExecuteNever));
  }
  surface(LAUNCHER);
  surface(WATCH);
  if (procs[WIFI].package)
    surface(WIFI);
  for (unsigned off = 0; off < 720 * 1600 * 4; off += 65536)
    device(DISPLAY, 0x79450000 + off, 0x08000000 + off, 16);
}
static void dismantle(void) {
  for (unsigned i = 1; i < COUNT; i++)
    if (procs[i].task.thread)
      check(seL4_TCB_Suspend(procs[i].task.thread));
  for (unsigned i = boot->untyped.start; i < boot->untyped.end; i++)
    check(seL4_CNode_Revoke(seL4_CapInitThreadCNode, i, 32));
}
void root_main(seL4_BootInfo *bi) {
  boot = bi;
  seL4_SetIPCBuffer(bi->ipcBuffer);
  for (;;) {
    wire();
#ifdef VIOLET_TEST_ISOLATION
    // Execute a single load in the launcher's real CSpace and VSpace.
    // Its fault endpoint is the same one used for production supervision.
    extern const unsigned probe_image[];
    seL4_UserContext regs = {.pc = BASE, .sp = STACK_TOP, .r0 = report[0]};
    fill_frame(procs[LAUNCHER].frames[0], (const unsigned char *)probe_image,
               8);
    check(seL4_TCB_WriteRegisters(procs[LAUNCHER].task.thread, 1, 0,
                                  sizeof(regs) / 4, &regs));
#else
    for (unsigned i = RUNTIME; i < COUNT; i++)
      if (i != WIFI && procs[i].package)
        start(i);
    start(LAUNCHER);
#endif
    for (;;) {
      unsigned badge = 0;
      seL4_MessageInfo_t msg = seL4_Recv(life, &badge);
      if (badge >= 16) {
#ifdef VIOLET_TEST_ISOLATION
        if (seL4_MessageInfo_get_label(msg) == seL4_Fault_VMFault &&
            seL4_GetMR(1) == report[0])
          report[1] = report[0];
        stop();
#endif
        report[4] = badge - 16;
        if (source == 1)
          source = 2;
        else if (source == 2)
          source = 0;
        break;
      }
      unsigned op = seL4_GetMR(0), key = seL4_GetMR(1), val = seL4_GetMR(2),
               result = 0, error = 2;
      for (unsigned i = 0; i < 14; i++)
        seL4_SetMR(i, 0);
      if (badge == PACKAGES && op == GET && key == INVENTORY) {
        error = 0;
        for (unsigned id = 1; id < COUNT; id++)
          if (id != PACKAGES && procs[id].package)
            result |= 1u << id;
      } else if (badge == RUNTIME && seL4_MessageInfo_get_length(msg) == 14) {
        if (op == EXECUTE && key == OPEN &&
            (val == LAUNCHER ||
             ((val == WATCH || val == WIFI) && procs[val].package))) {
          start(val);
          error = 0;
        } else if (op == EXECUTE && key == QUIT) {
          quit_watch();
          error = 0;
        }
      }
      seL4_SetMR(0, error);
      seL4_SetMR(2, result);
      seL4_Reply(seL4_MessageInfo_new(0, 0, 0, 14));
    }
    if (generation >= 5)
      stop();
    dismantle();
  }
}
#ifdef VIOLET_TEST_ISOLATION
const unsigned probe_image[] = {0xe5900000, 0xeafffffe};
#endif
