// Initial task. Child images have independent code, data, stack and CSpaces.
// Only this process holds untyped memory, child TCBs and their address spaces.
#include "../../platform/sel4/board/memory.h"
#include "tasks.h"

seL4_IPCBuffer *__sel4_ipc_buffer;
char __sel4_print_error;
extern const unsigned char builtin_catalog[], builtin_catalog_end[];
extern const struct package recovery_manager;
extern int memcmp(const void *, const void *, unsigned);
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
enum { FIRST_WORKER = COUNT, TOTAL = COUNT + 6, LANE_BASE = 0x07400000 };
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
static bool valid_package(const struct package *p, unsigned limit) {
  if (limit < sizeof(*p) || p->magic != 0x314b5056 ||
      p->abi != (p->id == RUNTIME ? 2u : 1u) || !p->id || p->id == PACKAGES ||
      p->id >= COUNT || !p->code || p->code % PAGE || p->size < p->code ||
      p->size > limit - sizeof(*p) ||
      p->size > ((p->id == RADIO || p->id == RUNTIME) ? 0x38000 : 0x3e000))
    return false;
  if (p->id >= 11) {
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
  return crc32(p->bytes, p->size) == p->crc;
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
    if (!valid_package(p, a->size - pos) || found[p->id])
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
  if (id == RADIO || id == RUNTIME) {
    check(pkg->size > 0x38000);
    for (unsigned i = 3; i <= 8; i++)
      private_page(p, STACK_TOP - i * PAGE, 0, 0, false);
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
  seL4_UserContext regs = {.pc = BASE, .sp = STACK_TOP, .r0 = id};
  check(seL4_TCB_WriteRegisters(p->task.thread, 1, 0,
                                sizeof(regs) / sizeof(unsigned), &regs));
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
    {TOUCH, 0x09000000, 0x09003000, 0},   {RADIO, 0x09010000, 0x09004000, 0},
    {STORAGE, 0x0a000000, 0x0a000000, 0}, {RADIO, 0x0a001000, 0x09001000, 0},
    {RADIO, 0x0a002000, 0x09003000, 0},   {0, 0x0a003000, 0x0a003000, 0},
    {TOUCH, 0x49000000, 0x09005000, 0},   {RADIO, 0x49001000, 0x09000000, 0},
    {POWER, 0x49002000, 0x09000000, 0},   {STORAGE, 0x49200000, 0x0b000000, 0},
    {RADIO, 0x49201000, 0x09002000, 0},   {RADIO, 0x49202000, 0x09005000, 0},
    {0, 0x49203000, 0x0b003000, 0}};
extern unsigned volume_load(void *destination);
extern bool volume_commit(const void *data, unsigned length, unsigned active);
extern bool volume_reject(unsigned active);
static unsigned volume_bank;
static const struct package *inbox;
static struct archive *local_catalog, *new_catalog;
static bool valid_inbox(void) { return valid_package(inbox, 1 << 20); }
static const char *title(const struct package *p) {
  static const char names[][16] = {"",      "LAUNCHER", "STOPWATCH",  "RUNTIME",
                                   "TOUCH", "DISPLAY",  "STORAGE",    "RADIO",
                                   "POWER", "",         "WI-FI SETUP"};
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
  if (!id || id >= COUNT || id == PACKAGES)
    return false;
  if (remove && (!procs[id].package || id == LAUNCHER || id == RUNTIME ||
                 id == TOUCH || id == DISPLAY))
    return false;
  if (!remove && (!valid_inbox() || inbox->id != id))
    return false;
  unsigned count = 0, length = 0;
  for (unsigned i = 1; i < COUNT; i++) {
    const struct package *p = i == id ? (remove ? 0 : inbox) : procs[i].package;
    if (!p || i == PACKAGES)
      continue;
    unsigned bytes = sizeof(*p) + p->size, aligned = (bytes + 3) & ~3u;
    if (length + aligned + 16 > 1 << 20)
      return false;
    memcpy(new_catalog->bytes + length, p, bytes);
    memset(new_catalog->bytes + length + bytes, 0, aligned - bytes);
    length += aligned;
    count++;
  }
  new_catalog->magic = 0x31544356;
  new_catalog->size = length;
  new_catalog->count = count;
  new_catalog->crc = crc32(new_catalog->bytes, length);
  return volume_commit(new_catalog, length + 16, volume_bank);
}
static void map_lane(seL4_CPtr frame, struct task *t, unsigned va) {
  ensure_table(t, va);
  unsigned copy = next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, 32,
                        seL4_CapInitThreadCNode, frame, 32, seL4_AllRights));
  check(
      seL4_ARM_Page_Map(copy, t->pd, va, seL4_AllRights,
                        seL4_ARM_Default_VMAttributes | seL4_ARM_ExecuteNever));
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
    if (lane == POWER)
      grant(&w->task, 4, procs[DISPLAY].ep, RUNTIME, false);
    grant(&w->task, slots[index], procs[lane].ep, RUNTIME, false);
    unsigned wake = create(ram, seL4_NotificationObject);
    grant(&w->task, 14, wake, 0, true);
    grant(&procs[RUNTIME].task, 9 + index, wake, 0, false);
  }
}
static void wire(void) {
  build_group();
  // Device untypeds must be consumed in ascending physical address order.
  for (unsigned i = 0; i < sizeof(devices) / sizeof(*devices); i++) {
    device(0, devices[i].pa,
           devices[i].id ? 0x0c000000 + i * PAGE : devices[i].va, 12);
    devices[i].frame = next_slot - 1;
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
  for (unsigned id = 1; id < COUNT; id++)
    if ((id == LAUNCHER || id == WATCH || id >= WIFI) && procs[id].package)
      grant(&procs[id].task, 1, procs[RUNTIME].ep, id, false);
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
  workers();
  surface(LAUNCHER);
  surface(WATCH);
  for (unsigned id = WIFI; id < COUNT; id++)
    if (procs[id].package)
      surface(id);
  for (unsigned off = 0; off < 720 * 1600 * 4; off += 65536)
    device(DISPLAY, 0x79450000 + off, 0x08000000 + off, 16);
}
static void dismantle(void) {
  for (unsigned i = 1; i < TOTAL; i++)
    if (procs[i].task.thread)
      check(seL4_TCB_Suspend(procs[i].task.thread));
  for (unsigned i = boot->untyped.start; i < boot->untyped.end; i++)
    check(seL4_CNode_Revoke(seL4_CapInitThreadCNode, i, 32));
}
unsigned ticks_per_ms;
unsigned ticks(void) {
  unsigned lo, hi;
  __asm__ volatile("isb; mrrc p15,1,%0,%1,c14" : "=r"(lo), "=r"(hi));
  return lo;
}
static unsigned stalled(unsigned timeout) {
  unsigned now = ticks();
  for (unsigned lane = TOUCH; lane <= PACKAGES; lane++) {
    if (!procs[lane].package)
      continue;
    volatile unsigned *box = (void *)(LANE_BASE + (lane - TOUCH) * PAGE);
    unsigned since = box[1020];
    if (since && (int)(now - since) > (int)timeout)
      return lane;
  }
  volatile unsigned *health = (void *)LANE_BASE;
  if ((int)(now - health[1021]) > (int)timeout)
    return RUNTIME;
  unsigned focus = health[1022];
  if (focus && (int)(now - health[1023]) > (int)timeout)
    return focus;
  return 0;
}
void root_main(seL4_BootInfo *bi) {
  unsigned freq;
  __asm__ volatile("mrc p15,0,%0,c14,c0,0" : "=r"(freq));
  ticks_per_ms = freq / 1000;
  unsigned faults = 0;
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
      if (i <= PACKAGES && procs[i].package)
        start(i);
    for (unsigned w = FIRST_WORKER; w < TOTAL; w++)
      if (procs[w].package)
        start(w);
    start(LAUNCHER);
    volatile unsigned *health = (void *)LANE_BASE;
    unsigned healthy_since = ticks();
    health[1021] = healthy_since;
    unsigned limit = freq * (report[5] == 1 ? 2 : 20);
    check(seL4_TCB_SetPriority(seL4_CapInitThreadTCB, seL4_CapInitThreadTCB,
                               100));
#endif
    for (;;) {
      unsigned badge = 0;
#ifdef VIOLET_TEST_ISOLATION
      seL4_MessageInfo_t msg = seL4_Recv(life, &badge);
#else
      seL4_MessageInfo_t msg = seL4_NBRecv(life, &badge);
      if (!badge) {
        unsigned id = stalled(limit);
        if (id)
          badge = 16 + id;
        else {
          if (ticks() - healthy_since > limit)
            faults = 0;
          seL4_Yield();
          continue;
        }
      }
#endif
      if (badge >= 16) {
#ifdef VIOLET_TEST_ISOLATION
        if (seL4_MessageInfo_get_label(msg) == seL4_Fault_VMFault &&
            seL4_GetMR(1) == report[0])
          report[1] = report[0];
        stop();
#endif
        if (++faults >= 5)
          stop();
        unsigned failed = badge - 16;
        report[4] = failed >= FIRST_WORKER ? RUNTIME : failed;
        if (source == 3 && !volume_reject(volume_bank))
          stop();
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
      if (badge == PACKAGES && op == EXECUTE && (key == 17 || key == 18)) {
        if (update_catalog(val, key == 18))
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
      seL4_Reply(seL4_MessageInfo_new(0, 0, 0, 14));
    }

    dismantle();
  }
}
#ifdef VIOLET_TEST_ISOLATION
const unsigned probe_image[] = {0xe5900000, 0xeafffffe};
#endif
