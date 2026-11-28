// Host test for the Ellis kernel extension after its policy moved to user
// level. It compiles the kernel's real invocation code (wifi/invoke.h, with
// the few kernel names it uses defined here) and the supervisor's real
// user-level half (wifi/user.h, wifi/windows.h) against a model of the
// MediaTek monitor, and checks
//  - the kernel accepts exactly the four operations of wifi/table.h, makes no
//    monitor call for anything else, and every monitor call it can make has
//    one of the fixed argument sets;
//  - over 1536 monitor states and requests, the new path issues the same
//    monitor calls in the same order, and gives the same result and raw
//    replies, as windows.h calling the monitor directly (test_ellis_reference.c).
// Built and run by `make test-unit`; under a second.
#include <assert.h>
#include <stdio.h>
#include <string.h>

// --- A model of the monitor, recording every call. ---
struct entry { unsigned sa, ea, apc[2]; };
static struct entry slots[32];
static int deny, primary_unavailable, offset_available, stale;
static unsigned random_reply[4];
struct call { unsigned function, x, y, z, w; };
static struct call trace[64];
static unsigned calls;
static void record(unsigned function, unsigned x, unsigned y, unsigned z, unsigned w) {
  assert(calls < 64);
  trace[calls++] = (struct call){function, x, y, z, w};
}
static unsigned set_region(unsigned start, unsigned end, unsigned permissions) {
  if (deny) return ~0u;
  struct entry *e = &slots[(start >> 24) & 31];
  if (e->apc[0] >> 31) return ~0u;  // locked
  e->sa = (start & 0xffff) - 0x4000;
  e->ea = (end & 0xffff) - 0x4000;
  e->apc[(end >> 24) & 1] = permissions;
  return 0;
}
unsigned monitor_call(unsigned operation, unsigned x, unsigned y, unsigned z) {
  record(1, operation, x, y, z);
  if (primary_unavailable) return ~0u;
  if (operation == 2) {
    const struct entry *e = &slots[y & 31];
    return x == 0 ? e->sa : x == 1 ? e->ea : e->apc[z & 1];
  }
  return operation == 0 ? set_region(x, y, z) : ~0u;
}
unsigned monitor_read_offset(unsigned offset) {
  record(2, offset, 0, 0, 0);
  if (!offset_available) return ~0u;
  if (stale) return 0;
  const struct entry *e = &slots[((offset & 0xff) / 4) & 31];
  return offset < 0x200 ? e->sa : offset < 0x300 ? e->ea : e->apc[((offset >> 8) - 3) & 1];
}
unsigned monitor_set_window(unsigned start, unsigned end, unsigned permissions) {
  record(3, start, end, permissions, 0);
  return offset_available ? set_region(start, end, permissions) : ~0u;
}
void monitor_random(unsigned words[4]) {
  record(4, 0, 0, 0, 0);
  memcpy(words, random_reply, sizeof random_reply);
}

// --- The kernel's side: invoke.h, with the kernel names it uses. ---
typedef unsigned word_t;
typedef int bool_t;
typedef int exception_t;
typedef struct { word_t reg[8]; int running; } tcb_t;
enum { EXCEPTION_NONE, EXCEPTION_SYSCALL_ERROR };
enum { seL4_IllegalOperation = 3 };
enum { badgeRegister = 4, msgInfoRegister = 5 };
enum { ThreadState_Running = 1 };
static const unsigned msgRegisters[] = {0, 1, 2, 3};
static struct { word_t type; } current_syscall_error;
static tcb_t current;
#define NODE_STATE(x) (&current)
static word_t getSyscallArg(word_t i, word_t *buffer) { (void)buffer; assert(i < 4); return current.reg[i]; }
static void setRegister(tcb_t *t, unsigned r, word_t v) { t->reg[r] = v; }
static void setThreadState(tcb_t *t, int s) { t->running = s; }
typedef struct { word_t words[1]; } seL4_MessageInfo_t;
static seL4_MessageInfo_t seL4_MessageInfo_new(word_t label, word_t caps, word_t extra, word_t length) {
  (void)caps; (void)extra;
  return (seL4_MessageInfo_t){{label << 12 | length}};
}
static word_t wordFromMessageInfo(seL4_MessageInfo_t info) { return info.words[0]; }
#define moto_emi_call monitor_call
#define moto_emi_read_offset monitor_read_offset
#define moto_emi_set_window monitor_set_window
#define moto_random_words monitor_random
#include "../../platform/sel4/board/wifi/invoke.h"
#undef moto_emi_call
#undef moto_emi_read_offset
#undef moto_emi_set_window
#undef moto_random_words

// One capability invocation, as the supervisor's seL4_Call sees it.
static int invoke(unsigned label, unsigned length, unsigned a, unsigned b, unsigned reply[4], int call) {
  current = (tcb_t){{a, b, 0xdead, 0xbeef, 0x77, 0}, 0};
  current_syscall_error.type = 0;
  if (ellisWifiInvocation(label, length, call) != EXCEPTION_NONE) {
    assert(current_syscall_error.type == seL4_IllegalOperation && !current.running);
    return -1;
  }
  assert(current.running == ThreadState_Running && current.reg[badgeRegister] == 0);
  assert(current.reg[msgInfoRegister] >> 12 == 0);
  unsigned count = current.reg[msgInfoRegister] & 0x7f;
  assert(count <= 4);
  for (unsigned i = 0; i < count; ++i) reply[i] = current.reg[i];
  return (int)count;
}

// --- The supervisor's side: user.h over that invocation. ---
static int ellis_smc(unsigned label, unsigned length, unsigned a, unsigned b, unsigned reply[4]) {
  return invoke(label, length, a, b, reply, 1);
}
#include "../../platform/sel4/board/wifi/user.h"

unsigned reference_request(int prepare, unsigned reader, unsigned raw[8]);
unsigned reference_permissions(unsigned group);

static int permitted_monitor_call(const struct call *c) {
  const unsigned end = (0x42000000u + 0x700000u - 1) >> 16, start = (25u << 24) | 0x4200;
  const unsigned high = reference_permissions(1), low = reference_permissions(0) | 1u << 31;
  int set = c->x == start && ((c->y == (end | 1u << 24) && c->z == high) || (c->y == end && c->z == low));
  switch (c->function) {
  case 1:
    if (c->x == 2) return c->y <= 2 && (c->z == 25 || c->z == 29) && c->w <= 1 && (c->y == 2 || !c->w);
    return c->x == 0 && c->y == start &&
           ((c->z == (end | 1u << 24) && c->w == high) || (c->z == end && c->w == low));
  case 2:
    return c->x >= 0x100 && c->x < 0x500 && ((c->x & 0xff) == 25 * 4 || (c->x & 0xff) == 29 * 4);
  case 3: return set;
  default: return c->function == 4;
  }
}

static void kernel_accepts_exactly_the_table(void) {
  assert(ELLIS_PERMISSIONS_HIGH == reference_permissions(1) && ELLIS_PERMISSIONS_LOW == reference_permissions(0));
  static const unsigned values[] = {0, 1, 2, 3, 4, 5, 24, 25, 26, 28, 29, 30, 31, 0x64, 0x74, 0x100, 0x4200,
                                    0x42000000, 0x7fffffff, 0x80000000, 0xffffffff};
  enum { N = sizeof values / sizeof values[0] };
  unsigned accepted = 0, refused = 0, reply[4];
  for (unsigned label = 0; label < 0x1000; ++label)
    for (unsigned length = 0; length < 6; ++length)
      for (unsigned i = 0; i < N; ++i)
        for (unsigned j = 0; j < N; ++j) {
          unsigned a = values[i], b = values[j];
          int expected = label == ELLIS_RANDOM ? length == 0
                       : length != 2 ? 0
                       : label == ELLIS_EMI_READ || label == ELLIS_EMI_READ_OFFSET ? a <= 3 && (b == 25 || b == 29)
                       : label == ELLIS_EMI_SET ? a <= 1 && b <= 1 : 0;
          calls = 0;
          int count = invoke(label, length, a, b, reply, 1);
          if (!expected) {
            assert(count == -1 && calls == 0);
            refused++;
            continue;
          }
          accepted++;
          assert(count == (label == ELLIS_RANDOM ? 4 : 1) && calls == 1 && permitted_monitor_call(&trace[0]));
          // The same request sent without a reply (not a Call) is refused and reaches no monitor.
          calls = 0;
          assert(invoke(label, length, a, b, reply, 0) == -1 && calls == 0);
        }
  // 8 reads by each reader, 4 sets, and the random call whatever its registers hold.
  assert(accepted == 8 + 8 + 4 + N * N);
  assert(refused == 0x1000 * 6 * N * N - accepted);
  // Label numbers outside the 12 bits swept above, including the old interface's 1, 2 and 3.
  for (unsigned label = 1; label; label <<= 1)
    if (label < ELLIS_EMI_READ || label > ELLIS_RANDOM) assert(invoke(label, 2, 0, 25, reply, 1) == -1);
}

static void set_slots(unsigned radio, unsigned secure) {
  static const struct entry programmed = {0x200, 0x26f, {0x00b6da28u | 1u << 31, 0x00b6db6d}};
  static const struct entry tee = {0x3fc0, 0x3fdf, {0xb69b69, 0xb69d6d}};
  memset(slots, 0, sizeof slots);
  if (radio == 1) slots[25] = programmed;
  if (radio == 2) slots[25].apc[0] = 0x123;
  if (radio == 3) memset(&slots[25], 0xff, sizeof slots[25]);
  if (secure == 1) slots[29] = tee;
  if (secure == 2) slots[29] = (struct entry){0x245, 0x245, {1, 1}};
  if (secure == 3) slots[29] = (struct entry){0x400, 0x3ff, {1, 1}};
}

static void same_monitor_calls_as_before(void) {
  unsigned scenarios = 0, programmed = 0, writes = 0;
  for (unsigned n = 0; n < 4 * 4 * 2 * 2 * 2 * 2 * 2 * 3; ++n) {
    unsigned v = n, radio = v % 4, secure = (v /= 4) % 4;
    deny = (v /= 4) % 2, primary_unavailable = (v /= 2) % 2, offset_available = (v /= 2) % 2, stale = (v /= 2) % 2;
    int prepare = (v /= 2) % 2;
    unsigned reader = (v /= 2) % 3;
    struct call before[64];
    struct entry after[32];
    unsigned raw_before[8], raw[8];
    set_slots(radio, secure);
    calls = 0;
    unsigned expected = reference_request(prepare, reader, raw_before);
    unsigned expected_calls = calls;
    memcpy(before, trace, sizeof before);
    memcpy(after, slots, sizeof after);

    set_slots(radio, secure);
    calls = 0;
    ellis_smc_refused = 0;
    unsigned result = radio_windows_request(prepare, reader);
    for (unsigned i = 0; i < 8; ++i) raw[i] = radio_window_raw[i / 4][i % 4];
    if (result != expected || calls != expected_calls || memcmp(before, trace, calls * sizeof trace[0]) ||
        memcmp(raw, raw_before, sizeof raw) || memcmp(after, slots, sizeof after) || ellis_smc_refused) {
      printf("scenario %u: result %#x (reference %#x), %u calls (reference %u), %u refused\n", n, result, expected,
             calls, expected_calls, ellis_smc_refused);
      assert(0);
    }
    for (unsigned i = 0; i < calls; ++i) {
      assert(permitted_monitor_call(&trace[i]));
      writes += trace[i].function == 3 || (trace[i].function == 1 && trace[i].x == 0);
    }
    scenarios++;
    programmed += result == 1 && radio == 0;
  }
  // The sweep reached the programming path, not only refusals.
  assert(scenarios == 1536 && programmed > 0 && writes > 0);
}

static void user_side_refuses_what_the_kernel_would_not_do(void) {
  // windows.h asking for anything but the kernel's own two steps is an error
  // here, not a silently different monitor call.
  calls = 0, ellis_smc_refused = 0;
  assert(moto_emi_set_window(ELLIS_SET_START, ELLIS_SET_END, ELLIS_PERMISSIONS_HIGH) == ~0u);
  assert(moto_emi_set_window(ELLIS_SET_START + 1, ELLIS_SET_END, ELLIS_PERMISSIONS_LOW | 1u << 31) == ~0u);
  assert(moto_emi_call(EMI_SET, (26u << 24) | 0x4200, ELLIS_SET_END, ELLIS_PERMISSIONS_LOW | 1u << 31) == ~0u);
  assert(moto_emi_call(EMI_READ, 0, 25, 1) == ~0u && moto_emi_call(EMI_READ, 3, 25, 0) == ~0u);
  assert(moto_emi_call(1, 0, 25, 0) == ~0u && moto_emi_read_offset(0x500 + 25 * 4) == ~0u);
  assert(moto_emi_read_offset(0x100 + 25 * 4 + 1) == ~0u && moto_emi_read_offset(25 * 4) == ~0u);
  assert(calls == 0 && ellis_smc_refused == 9);
  // A region the kernel does not serve: the kernel refuses, and that is counted.
  assert(moto_emi_call(EMI_READ, 0, 26, 0) == ~0u && moto_emi_read_offset(0x100 + 24 * 4) == ~0u);
  assert(calls == 0 && ellis_smc_refused == 11);
}

static void random_words_are_filtered_at_user_level(void) {
  static const struct { unsigned reply[4]; int usable; } cases[] = {
      {{0x11111111, 0x22222222, 0x33333333, 0x44444444}, 1},
      {{0, 0x22222222, 0x33333333, 0x44444444}, 1},
      {{0xffffffff, 0x74726e67, 0, 0}, 0},  // an error status and the untouched arguments
      {{0, 0, 0, 0}, 0},                    // the QEMU fixture
      {{5, 7, 7, 7}, 0},
      {{5, 7, 0, 9}, 0},
  };
  for (unsigned i = 0; i < sizeof cases / sizeof cases[0]; ++i) {
    unsigned words[4] = {0};
    memcpy(random_reply, cases[i].reply, sizeof random_reply);
    calls = 0;
    assert(ellis_random(words) == cases[i].usable && calls == 1 && trace[0].function == 4);
    assert(!memcmp(words, cases[i].reply, sizeof words));
  }
}

int main(void) {
  kernel_accepts_exactly_the_table();
  same_monitor_calls_as_before();
  user_side_refuses_what_the_kernel_would_not_do();
  random_words_are_filtered_at_user_level();
  puts("ellis kernel extension: table, 1536 monitor scenarios, user-level checks and entropy filter ok");
  return 0;
}
