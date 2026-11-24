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
// Audio follows USB Diagnostics so app and service identities keep their values.
// QEMU has no USB Diagnostics process; its slot stays empty there.
// The Open Runtime (24) hosts Linux-ABI apps from /apps. Slot 23 is left for Bluetooth.
enum { FIRST_WORKER = COUNT, USB_DIAG = COUNT + 6, AUDIO = COUNT + 7, OPEN_RT = COUNT + 9,
       USB_TRANSFER_BADGE = 14, TOTAL = OPEN_RT + 1, LANE_BASE = 0x07400000,
       RUNTIME_AUDIO_SLOT = 2, AUDIO_POWER_SLOT = 6,
       // Badges 16 and up mean faults, so Audio's supervisor requests use an
       // app identity that never holds the lifecycle endpoint.
       AUDIO_LIFE_BADGE = 12, AUDIO_PLATFORM = 0x4071 };
// The Open Runtime's CSpace and the capabilities the supervisor places in it.
// Runtime knows it as client 9, an identity no catalog app uses.
enum { OPEN_BITS = 14, OPEN_RUNTIME_BADGE = 9, OPEN_STACK_TOP = 0x02c00000, OPEN_STACK_PAGES = 16,
       OPEN_SLOT_RUNTIME = 2, OPEN_SLOT_CNODE = 3, OPEN_SLOT_PD = 4, OPEN_SLOT_ASID = 5,
       OPEN_SLOT_KERNEL = 6, OPEN_SLOT_AUTHORITY = 7, OPEN_SLOT_POOL = 16, OPEN_KERNEL_BITS = 22 };
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
  if (limit < sizeof(*p)) return false;
  bool abi_ok=p->abi==(p->id==RUNTIME ? 2u : 1u);
#ifdef VIOLET_MOTO
  if (p->id==RADIO && p->abi==3) abi_ok=true;
#endif
  bool allowed_id = p->id < COUNT || p->id == AUDIO || p->id == OPEN_RT;
#ifdef VIOLET_MOTO
  allowed_id |= p->id == USB_DIAG;
#endif
  if (p->magic != 0x314b5056 ||
      !abi_ok || !p->id || p->id == PACKAGES ||
      !allowed_id || !p->code || p->code % PAGE || p->size < p->code ||
      p->size > limit - sizeof(*p) ||
      p->size > (p->id == RUNTIME ? 0x38000 : 0x3e000))
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
  return crc32(p->bytes, p->size) == p->crc;
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
    pos += (sizeof(*p) + p->size + 3) & ~3u;
    if (pos > a->size)
      return false;
    found[p->id] = p;
  }
#ifdef VIOLET_MOTO
  if (!found[STORAGE] || !found[RADIO] || !found[POWER]) return false;
#endif
  if (pos != a->size)
    return false;
  for (unsigned i = 1; i < COUNT; i++)
    if ((i == LAUNCHER || i == RUNTIME || i == TOUCH || i == DISPLAY) &&
        !found[i])
      return false;
  for (unsigned i = 1; i < COUNT; i++)
    procs[i].package = found[i];
#ifdef VIOLET_MOTO
  // Older installed catalogs inherit Diagnostics from the factory catalog.
  if (found[USB_DIAG]) procs[USB_DIAG].package = found[USB_DIAG];
#endif
  procs[AUDIO].package = found[AUDIO];
  procs[OPEN_RT].package = found[OPEN_RT];
  return true;
}
static void grant(struct task *t, unsigned slot, seL4_CPtr ep, unsigned badge,
                  bool server) {
  check(seL4_CNode_Mint(t->caps, slot, t->cap_bits ? t->cap_bits : CAP_BITS, seL4_CapInitThreadCNode, ep,
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
  else if (va >= BASE && va < STACK_TOP)
    p->frames[(va - BASE) / PAGE] = frame;
}
// Storage uses nested 512-byte sector buffers. Keep its stack separate from
// the package image, with an unmapped guard page below it. Radio's stack is
// separate too, so its package may use the full 0x3e000-byte image window.
enum { STORAGE_STACK_TOP = 0x02400000, STORAGE_STACK_PAGES = 8 };
enum { RADIO_STACK_TOP = 0x02600000, RADIO_STACK_PAGES = 8 };
static unsigned stack_top(unsigned id) {
  return id == STORAGE ? STORAGE_STACK_TOP : id == RADIO ? RADIO_STACK_TOP
         : id == OPEN_RT ? OPEN_STACK_TOP : STACK_TOP;
}
static seL4_CPtr spare_ram(unsigned type, unsigned bits);
static void prepare_process(unsigned id) {
  struct process *p = &procs[id];
  const struct package *pkg = p->package;
  if (!pkg)
    return;
  struct task *t = &p->task;
  t->pd = create(ram, seL4_ARM_PageDirectoryObject);
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
#ifdef VIOLET_MOTO
  if (pkg->id==RADIO && pkg->abi==3)
    for (unsigned off=0;off<0x20000;off+=PAGE) private_page(p,0x02800000+off,0,0,false);
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
    private_page(p, STACK_TOP - 2 * PAGE, 0, 0, false);
    private_page(p, STACK_TOP - PAGE, 0, 0, false);
  }
  private_page(p, IPC_BUFFER, 0, 0, false);
  grant(t, FAULT_SLOT, life, 16 + id, false);
  unsigned guard = seL4_CNode_CapData_new(0, 32 - t->cap_bits).words[0];
  check(seL4_TCB_Configure(t->thread, FAULT_SLOT, t->caps, guard, t->pd, 0,
                           IPC_BUFFER, t->ipc));
  check(seL4_TCB_SetPriority(t->thread, seL4_CapInitThreadTCB, 100));
}
static void start(unsigned id) {
  struct process *p = &procs[id];
  if (p->running)
    return;
  seL4_UserContext regs = {.pc = BASE, .sp = stack_top(id), .r0 = id};
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
#ifdef VIOLET_MOTO
// Share an already allocated device frame with the supervisor without
// allocating the same physical address from its untyped a second time.
static void board_device(unsigned id, unsigned pa, unsigned va, unsigned root_va) {
  device(id, pa, va, 12);
  unsigned frame = next_slot - 1;
  ensure_table(&bootstrap, root_va);
  unsigned copy = next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, 32,
                        seL4_CapInitThreadCNode, frame, 32, seL4_AllRights));
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
  boot_display_frame = next_slot - 1;
  boot_stage(0);
#endif
  const struct archive *factory = (const void *)builtin_catalog;
  check(!parse(factory, builtin_catalog_end - builtin_catalog));
}
#ifdef VIOLET_QEMU
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
    // Open apps' TCP sockets, served by the emulator's host relay (tools/net_relay.py).
    {OPEN_RT, 0x49230000, 0x09400000, 0},
    {OPEN_RT, 0x49231000, 0x09401000, 0},
    {OPEN_RT, 0x49232000, 0x09402000, 0},
    {OPEN_RT, 0x49233000, 0x09403000, 0},
    {OPEN_RT, 0x49234000, 0x09404000, 0},
    {OPEN_RT, 0x49235000, 0x09405000, 0},
    {OPEN_RT, 0x49236000, 0x09406000, 0},
    {OPEN_RT, 0x49237000, 0x09407000, 0},
    {OPEN_RT, 0x49238000, 0x09408000, 0},
    {OPEN_RT, 0x49239000, 0x09409000, 0},
    {OPEN_RT, 0x4923a000, 0x0940a000, 0},
    {OPEN_RT, 0x4923b000, 0x0940b000, 0},
    {OPEN_RT, 0x4923c000, 0x0940c000, 0},
    {OPEN_RT, 0x4923d000, 0x0940d000, 0},
    {OPEN_RT, 0x4923e000, 0x0940e000, 0},
    {OPEN_RT, 0x4923f000, 0x0940f000, 0},
    {OPEN_RT, 0x49240000, 0x09410000, 0},
    {OPEN_RT, 0x49241000, 0x09411000, 0},
    {OPEN_RT, 0x49242000, 0x09412000, 0},
    {OPEN_RT, 0x49243000, 0x09413000, 0},
    {OPEN_RT, 0x49244000, 0x09414000, 0},
    {OPEN_RT, 0x49245000, 0x09415000, 0},
    {OPEN_RT, 0x49246000, 0x09416000, 0},
    {OPEN_RT, 0x49247000, 0x09417000, 0},
    {OPEN_RT, 0x49248000, 0x09418000, 0},
    {OPEN_RT, 0x49249000, 0x09419000, 0},
    {OPEN_RT, 0x4924a000, 0x0941a000, 0},
    {OPEN_RT, 0x4924b000, 0x0941b000, 0},
    {OPEN_RT, 0x4924c000, 0x0941c000, 0},
    {OPEN_RT, 0x4924d000, 0x0941d000, 0},
    {OPEN_RT, 0x4924e000, 0x0941e000, 0},
    {OPEN_RT, 0x4924f000, 0x0941f000, 0},
    {OPEN_RT, 0x49250000, 0x09420000, 0},
    {OPEN_RT, 0x49251000, 0x09421000, 0},
    {OPEN_RT, 0x49252000, 0x09422000, 0},
    {OPEN_RT, 0x49253000, 0x09423000, 0},
    {OPEN_RT, 0x49254000, 0x09424000, 0},
    {OPEN_RT, 0x49255000, 0x09425000, 0},
    {OPEN_RT, 0x49256000, 0x09426000, 0},
    {OPEN_RT, 0x49257000, 0x09427000, 0},
    {OPEN_RT, 0x49258000, 0x09428000, 0},
    {OPEN_RT, 0x49259000, 0x09429000, 0},
    {OPEN_RT, 0x4925a000, 0x0942a000, 0},
    {OPEN_RT, 0x4925b000, 0x0942b000, 0},
    {OPEN_RT, 0x4925c000, 0x0942c000, 0},
    {OPEN_RT, 0x4925d000, 0x0942d000, 0},
    {OPEN_RT, 0x4925e000, 0x0942e000, 0},
    {OPEN_RT, 0x4925f000, 0x0942f000, 0},
    {OPEN_RT, 0x49260000, 0x09430000, 0}};
#else
static unsigned handset_report[6];
static unsigned uploaded, upload_size, upload_crc;
static seL4_CPtr catalog_candidate_frame;
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
static const struct package *inbox;
static struct archive *local_catalog, *new_catalog;
#ifdef VIOLET_MOTO
#include "bearssl.h"
#include "developer-key.h"
// USB provisioning lives only in supervisor RAM. Only Radio may retrieve it.
static unsigned char profile_name[32], profile_key[32];
static unsigned profile_length, profile_transaction, profile_epoch;
static unsigned long long profile_ticks;
static bool profile_pending, profile_ready;
static unsigned join_job;
static unsigned char package_signature[72];
static bool signature_started, package_authorized;
static bool signed_update(unsigned size) {
  if (!upload_size || uploaded != upload_size || (inbox->id != RADIO && inbox->id != USB_DIAG) ||
      !valid_package(inbox, uploaded)) return false;
  unsigned char digest[32];
  br_sha256_context hash;
  br_sha256_init(&hash);
  br_sha256_update(&hash, inbox, uploaded);
  br_sha256_out(&hash, digest);
  br_ec_public_key pk = {BR_EC_secp256r1, developer_key, sizeof(developer_key)};
  return br_ecdsa_i31_vrfy_asn1(&br_ec_prime_i31, digest, sizeof(digest), &pk,
                                package_signature, size);
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
                    open_name[16] = "OPEN RUNTIME";
  if (p->id == USB_DIAG) return diag_name;
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
  bool known = (id > 0 && id < COUNT && id != PACKAGES) || id == AUDIO || id == OPEN_RT;
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
  unsigned count = 0, length = 0;
  for (unsigned i = 1; i < TOTAL; i++) {
    if (i >= COUNT && i != USB_DIAG && i != AUDIO && i != OPEN_RT) continue;
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
  return true;
#else
  return volume_commit(new_catalog, length + 16, volume_bank);
#endif
}
static void map_shared(seL4_CPtr frame, struct task *t, unsigned va, seL4_CapRights_t rights) {
  ensure_table(t, va);
  unsigned copy = next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, 32,
                        seL4_CapInitThreadCNode, frame, 32, seL4_AllRights));
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
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, 32, seL4_CapInitThreadCNode, frame, 32,
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
// other processes share, so its app objects never crowd them out.
static seL4_CPtr spare_ram(unsigned type, unsigned bits) {
  for (unsigned i = 0; i < boot->untyped.end - boot->untyped.start; i++) {
    seL4_CPtr u = boot->untyped.start + i;
    if (boot->untypedList[i].isDevice || u == ram || boot->untypedList[i].sizeBits < bits) continue;
    if (next_slot >= boot->empty.end) stop();
    if (!seL4_Untyped_Retype(u, type, bits, seL4_CapInitThreadCNode, 0, 0, next_slot, 1))
      return next_slot++;
  }
  // Last, the shared untyped, but never at the cost of booting: 0 means none.
  if (!seL4_Untyped_Retype(ram, type, bits, seL4_CapInitThreadCNode, 0, 0, next_slot, 1))
    return next_slot++;
  return 0;
}
// How far Open Runtime setup got, and the root CNode's use, for the boot harness.
volatile unsigned open_setup[4], open_fault[6];
static void give_open(unsigned slot, seL4_CPtr cap) {
  check(seL4_CNode_Copy(procs[OPEN_RT].task.caps, slot, OPEN_BITS, seL4_CapInitThreadCNode, cap, 32,
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
  // Its own thread, allowed to give app threads the priority every process runs at.
  check(seL4_TCB_SetMCPriority(t->thread, seL4_CapInitThreadTCB, 100));
  give_open(OPEN_SLOT_AUTHORITY, t->thread);
  seL4_CPtr kernel_memory = spare_ram(seL4_UntypedObject, OPEN_KERNEL_BITS);
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
  // Open apps' TCP sockets: a control page and eight slots of 24 KiB of rings,
  // served by Radio at 0x0b000000, used by the Open Runtime at 0x09400000.
  for (unsigned i = 0; i < 49; i++) {
    unsigned frame = create(ram, seL4_ARM_SmallPageObject);
    map_shared(frame, &procs[RADIO].task, 0x0b000000 + i * PAGE, seL4_AllRights);
    map_shared(frame, t, 0x09400000 + i * PAGE, seL4_AllRights);
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
  grant(t, OPEN_SLOT_RUNTIME, procs[RUNTIME].ep, OPEN_RUNTIME_BADGE, false);
  open_setup[0] = 9;
  open_setup[1] = next_slot;
#ifdef VIOLET_MOTO
  for (unsigned i = 0; i < 2; i++) ((volatile unsigned *)0x09202000)[46 + i] = open_setup[i];
#endif
}
static void wire(void) {
  build_group();
#ifdef VIOLET_QEMU
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
#else
  report = handset_report;
  factory_usb = procs[USB_DIAG].package;
  check(!factory_usb);
  local_catalog = device(0,0x43c00000,0x0d000000,20);
  new_catalog = device(0,0x43d00000,0x0d100000,20);
  catalog_candidate_frame = next_slot-1;
  if (!generation) volume_bank = *(volatile unsigned *)0x0d1ffffc;
  source = volume_bank>=1 && volume_bank<=2 && parse(local_catalog,1<<20) ? 3 : 0;
  if (!source) volume_bank=0;
  unsigned upload_frame = create(ram,seL4_ARM_SectionObject);
  check(seL4_ARM_Page_Map(upload_frame,bootstrap.pd,0x0d200000,seL4_AllRights,
                          seL4_ARM_Default_VMAttributes|seL4_ARM_ExecuteNever));
  inbox = (const void *)0x0d200000;
  upload_size = uploaded = 0;
  package_authorized = signature_started = false;
#endif
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
  if (procs[AUDIO].package) {
    // Only Runtime calls Audio. On the handset Audio reaches the PMIC through Power.
    prepare_process(AUDIO);
    procs[AUDIO].ep = create(ram, seL4_EndpointObject);
    grant(&procs[AUDIO].task, 1, procs[AUDIO].ep, 0, true);
    grant(&procs[RUNTIME].task, RUNTIME_AUDIO_SLOT, procs[AUDIO].ep, RUNTIME, false);
    if (procs[POWER].package)
      grant(&procs[AUDIO].task, AUDIO_POWER_SLOT, procs[POWER].ep, AUDIO, false);
    // Raw PCM from open apps: a header page and a 16-page ring, at 0x09300000
    // in Audio and 0x09100000 in the Open Runtime.
    for (unsigned i = 0; i < 17; i++) {
      unsigned frame = create(ram, seL4_ARM_SmallPageObject);
      map_shared(frame, &procs[AUDIO].task, 0x09300000 + i * PAGE, seL4_AllRights);
      if (procs[OPEN_RT].task.pd)
        map_shared(frame, &procs[OPEN_RT].task, 0x09100000 + i * PAGE, seL4_AllRights);
    }
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
    check(seL4_ARM_Page_Unmap(d.frame));
    ensure_table(t, d.va);
    check(seL4_ARM_Page_Map(d.frame, t->pd, d.va, seL4_AllRights,
                            seL4_ARM_ExecuteNever));
  }
#else
  boot_stage(1);
  unsigned copy_catalog=next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode,copy_catalog,32,seL4_CapInitThreadCNode,
                        catalog_candidate_frame,32,seL4_CanRead));
  check(seL4_ARM_Page_Map(copy_catalog,procs[STORAGE].task.pd,0x0d100000,seL4_CanRead,seL4_ARM_ExecuteNever));
  if (usb_factory_only) procs[USB_DIAG].package = factory_usb;
  if (usb_trial_bytes) {
    // The scratch section survives group teardown. The inbox does not.
    memcpy((void *)inbox, new_catalog, usb_trial_bytes);
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
  unsigned spm_frame = next_slot - 1;
  ensure_table(&procs[RADIO].task, 0x09000000);
  unsigned spm_copy = next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, spm_copy, 32, seL4_CapInitThreadCNode,
                        spm_frame, 32, seL4_CanRead));
  check(seL4_ARM_Page_Map(spm_copy, procs[RADIO].task.pd, 0x09000000,
                          seL4_CanRead, seL4_ARM_ExecuteNever));
  grant(&procs[RADIO].task, 7, life, RADIO, false);
  board_device(TOUCH, 0x10007000, 0x09005000, 0x09703000);
  device(POWER, 0x1000d000, 0x09000000, 12);
  if (procs[AUDIO].package) {
    // Keypad (Volume Down), read-only: Audio cannot reconfigure the scan.
    mapping = &procs[AUDIO].task;
    map_device_rights(0x10010000, 0x09000000, 12, seL4_CanRead);
    map_device(0x11007000, 0x09004000, 12); // I2C0: FS1603 speaker amplifier
  }
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
  device(USB_DIAG, 0x11cc0000, 0x09001000, 12);
  board_device(RADIO, 0x18002000, 0x09015000, 0x09706000); // connectivity MCU and ROM mailbox
  device(RADIO, 0x18004000, 0x09011000, 12);
  device(RADIO,0x18006000,0x09014000,12); // WLAN DMA packet scheduler
  board_device(RADIO,0x18007000,0x09012000,0x09704000); // always-on connectivity clock check
  mapping = &bootstrap;
  map_device_rights(0x180b1000, 0x09705000, 12, seL4_CanRead);
  board_device(RADIO,0x180c1000,0x09013000,0x09707000); // shared oscillator control
  mapping = &procs[RADIO].task;
  for (unsigned off = 0; off < 0x300000; off += 0x100000)
    map_device(0x42400000 + off, 0x09c00000 + off, 20);
  for (unsigned off = 0; off < 0x400000; off += 0x100000)
    map_device(0x42000000 + off, 0x09800000 + off, 20);
  for (unsigned off = 0; off < 0x200000; off += 0x100000)
    map_device_rights(0x43e00000 + off, 0x09400000 + off, 20, seL4_CanRead);
  unsigned policy_cap=next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode,policy_cap,32,seL4_CapInitThreadCNode,
                        policy_cap-1,32,seL4_CanRead));
  check(seL4_ARM_Page_Map(policy_cap,bootstrap.pd,0x09f00000,seL4_CanRead,seL4_ARM_ExecuteNever));
  // Touch reads the loader's panel word at 0x43fffff0 to pick its firmware.
  unsigned touch_boot=next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode,touch_boot,32,seL4_CapInitThreadCNode,
                        policy_cap,32,seL4_CanRead));
  check(seL4_ARM_Page_Map(touch_boot,procs[TOUCH].task.pd,0x09f00000,seL4_CanRead,seL4_ARM_ExecuteNever));
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
  open_runtime();
  unsigned first = 0;
#ifdef VIOLET_MOTO
  struct task *display = &procs[DISPLAY].task;
  ensure_table(display, 0x08000000);
  unsigned copy = next_slot++;
  check(seL4_CNode_Copy(seL4_CapInitThreadCNode, copy, 32, seL4_CapInitThreadCNode,
                        boot_display_frame, 32, seL4_AllRights));
  check(seL4_ARM_Page_Map(copy, display->pd, 0x08000000, seL4_AllRights, seL4_ARM_ExecuteNever));
  share_scanout_with_open(boot_display_frame, 0x08000000);
  first = 65536;
#endif
  for (unsigned off = first; off < 720 * 1600 * 4; off += 65536) {
    device(DISPLAY, 0x79450000 + off, 0x08000000 + off, 16);
    // The Open Runtime draws the app in front straight to scanout.
    share_scanout_with_open(next_slot - 1, 0x08000000 + off);
  }
}
static void dismantle(void) {
  for (unsigned i = 1; i < TOTAL; i++)
    if (procs[i].task.thread)
      check(seL4_TCB_Suspend(procs[i].task.thread));
  for (unsigned i = boot->untyped.start; i < boot->untyped.end; i++)
    check(seL4_CNode_Revoke(seL4_CapInitThreadCNode, i, 32));
}
unsigned ticks_per_ms;
static unsigned long long ticks64(void) {
  unsigned lo, hi;
  __asm__ volatile("isb; mrrc p15,1,%0,%1,c14" : "=r"(lo), "=r"(hi));
  return ((unsigned long long)hi << 32) | lo;
}
unsigned ticks(void) { return (unsigned)ticks64(); }
static unsigned stalled(unsigned timeout) {
  unsigned now = ticks();
  for (unsigned lane = TOUCH; lane <= PACKAGES; lane++) {
    if (!procs[lane].package)
      continue;
    volatile unsigned *box = (void *)(LANE_BASE + (lane - TOUCH) * PAGE);
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
static unsigned wifi_memory_request(bool prepare, unsigned reader) {
  volatile unsigned *diag = (void *)0x09202000;
  volatile unsigned *spm = (void *)0x09700000;
  if (prepare) {
    unsigned state = (spm[0x180/4] | spm[0x184/4]) & ((1u<<1) | (1u<<30));
    if (conn_saved.powered || conn_saved.live || conn_saved.remapped || state) return 0x30;
  }
  unsigned profile = *(volatile unsigned *)0x09fffff8;
  if (prepare && profile != 1 && profile != 2) return 0x31;
  // The Ellis AArch32 kernel installs its board-only cap in the reserved
  // SMC slot. No child owns this cap. The sole argument selects a bounded reader.
  seL4_SetMR(0, reader);
  seL4_MessageInfo_t reply = seL4_Call(seL4_CapSMC, seL4_MessageInfo_new(prepare ? 2 : 1,0,0,1));
  if (seL4_MessageInfo_get_label(reply) || seL4_MessageInfo_get_length(reply) != 9)
    return 0x43;
  unsigned policy = seL4_GetMR(0);
  for (unsigned i = 0; i < 8; ++i) diag[18+i] = seL4_GetMR(1+i);
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
#endif
void root_main(seL4_BootInfo *bi) {
  unsigned freq;
  __asm__ volatile("mrc p15,0,%0,c14,c0,0" : "=r"(freq));
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
#ifdef VIOLET_MOTO
    boot_stage(3);
#endif
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
          if (ticks() - healthy_since > limit)
            faults = 0;
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
        check(seL4_TCB_Suspend(procs[OPEN_RT].task.thread));
        continue;
      }
      if (badge >= 16) {
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
        if (source == 3) {
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
        if (source == 3 && !volume_reject(volume_bank))
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
          seL4_Reply(seL4_MessageInfo_new(0,0,0,5));
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
          seL4_Reply(seL4_MessageInfo_new(0,0,0,1));
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
            if (inbox->id == USB_DIAG) {
              usb_trial_bytes = uploaded;
              memcpy(new_catalog,inbox,usb_trial_bytes);
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
          seL4_Reply(seL4_MessageInfo_new(0,0,0,14));
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
          if(val>=sizeof(*inbox) && val<=sizeof(*inbox)+0x3e000) {
            upload_size=val;upload_crc=n;ok=1;
          }
        } else if(key==0x61 && n && n<=40 && val==uploaded && n<=upload_size-uploaded) {
          unsigned words[10];
          for(unsigned i=0;i<10;i++) words[i]=seL4_GetMR(4+i);
          memcpy((unsigned char *)inbox+uploaded,words,n);
          uploaded+=n;ok=1;
          if(uploaded==upload_size && (crc32((const unsigned char*)inbox,uploaded)!=upload_crc ||
              !valid_package(inbox,uploaded) || sizeof(*inbox)+inbox->size!=uploaded)) {
            upload_size=uploaded=0;ok=0;
          }
        } else if(key==0x62) { ok=1; }
        seL4_SetMR(0,ok?0:2);seL4_SetMR(1,upload_size);seL4_SetMR(2,uploaded);
        seL4_SetMR(3,procs[RADIO].package?procs[RADIO].package->crc:0);
        seL4_SetMR(4,(key==0x62 && valid_inbox())?inbox->id:0);seL4_SetMR(5,volume_bank);
        seL4_Reply(seL4_MessageInfo_new(0,0,0,14));continue;
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
        seL4_Reply(seL4_MessageInfo_new(0,0,0,14));continue;
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
        seL4_Reply(seL4_MessageInfo_new(0,0,0,14));
        continue;
      }
#endif
      for (unsigned i = 0; i < 14; i++)
        seL4_SetMR(i, 0);
#ifdef VIOLET_MOTO
      if (badge == RADIO && key == 0x405e && op == GET && profile_ready &&
          seL4_MessageInfo_get_length(msg) == 14) {
        unsigned freq;
        __asm__ volatile("mrc p15,0,%0,c14,c0,0" : "=r"(freq));
        if (freq) {
          unsigned long long elapsed=(ticks64()-profile_ticks)/freq;
          if (elapsed <= 0xffffffffu-profile_epoch) { result=profile_epoch+(unsigned)elapsed; error=0; }
        }
      } else if (badge == RADIO && key == 0x405c && op == GET &&
          seL4_MessageInfo_get_length(msg) == 14) {
        seL4_MessageInfo_t entropy = seL4_Call(seL4_CapSMC, seL4_MessageInfo_new(3,0,0,0));
        if (!seL4_MessageInfo_get_label(entropy) && seL4_MessageInfo_get_length(entropy) == 4) {
          unsigned words[4];
          for (unsigned i=0;i<4;i++) words[i]=seL4_GetMR(i);
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
const unsigned probe_image[] = {0xe5900000, 0xeafffffe};
#endif
