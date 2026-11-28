// User-level half of the Ellis kernel extension, for the supervisor (the only
// holder of the capability). windows.h decides when the connectivity window
// may be inspected or programmed and what the monitor's replies mean; this
// file carries each of its monitor calls to the kernel as one of the four
// permitted operations (table.h). The includer provides
//
//     int ellis_smc(unsigned label, unsigned length, unsigned a, unsigned b, unsigned reply[4]);
//
// which invokes the capability and returns the number of reply words, or -1
// when the kernel refused the call.
#include "table.h"
#include "windows.h"

static int ellis_smc(unsigned label, unsigned length, unsigned a, unsigned b, unsigned reply[4]);
// Set when the kernel refused a call: a mismatch between this file and the
// kernel, never a monitor reply.
static unsigned ellis_smc_refused;
static unsigned moto_emi_set(unsigned start, unsigned end, unsigned permissions, unsigned interface);

static unsigned ellis_word(unsigned label, unsigned a, unsigned b) {
  unsigned reply[4];
  if (ellis_smc(label, 2, a, b, reply) != 1) {
    ellis_smc_refused++;
    return ~0u;
  }
  return reply[0];
}
// windows.h reads register i of a region as (i < 2 ? i : 2, region, i == 3).
unsigned moto_emi_call(unsigned operation, unsigned x, unsigned y, unsigned z) {
  if (operation == EMI_READ) {
    if (x > 2 || z > 1 || (x < 2 && z)) return ellis_smc_refused++, ~0u;
    return ellis_word(ELLIS_EMI_READ, x + z, y);
  }
  return operation == EMI_SET ? moto_emi_set(x, y, z, 0) : (ellis_smc_refused++, ~0u);
}
unsigned moto_emi_read_offset(unsigned offset) {
  unsigned field = (offset >> 8) - 1, region = (offset & 0xff) / 4;
  if (offset & 3 || field > 3) return ellis_smc_refused++, ~0u;
  return ellis_word(ELLIS_EMI_READ_OFFSET, field, region);
}
unsigned moto_emi_set_window(unsigned start, unsigned end, unsigned permissions) {
  return moto_emi_set(start, end, permissions, 1);
}
// The kernel programs only its own two steps: anything else windows.h might
// ask for is refused here rather than silently replaced.
static unsigned moto_emi_set(unsigned start, unsigned end, unsigned permissions, unsigned interface) {
  unsigned m[4];
  for (unsigned step = 0; step < 2; ++step) {
    ellis_smc_arguments(ELLIS_EMI_SET, 2, step, interface, m);
    if (m[0] == start && m[1] == end && m[2] == permissions)
      return ellis_word(ELLIS_EMI_SET, step, interface);
  }
  return ellis_smc_refused++, ~0u;
}
// Four words from the secure random number generator, or 0. Firmware error
// replies return one status word: never use those as entropy.
static int ellis_random(unsigned words[4]) {
  if (ellis_smc(ELLIS_RANDOM, 0, 0, 0, words) != 4) return 0;
  return words[1] && words[2] && words[3] && !(words[1] == words[2] && words[2] == words[3]);
}
