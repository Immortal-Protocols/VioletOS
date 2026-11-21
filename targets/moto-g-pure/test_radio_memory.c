#include "../../platform/sel4/board/wifi/windows.h"
#include <assert.h>
#include <string.h>
struct entry {
  unsigned sa, ea, apc[2];
};
static struct entry slots[32];
static unsigned writes, offset_writes;
static int stale;
static int deny, primary_unavailable, offset_available;
unsigned moto_emi_read_offset(unsigned offset) {
  if (!offset_available) return ~0u;
  unsigned region = (offset & 0xff) / 4;
  if (stale) return 0;
  assert(region == 25 || region == 29);
  struct entry *e = &slots[region];
  return offset < 0x200 ? e->sa : offset < 0x300 ? e->ea : e->apc[(offset >> 8) - 3];
}
static unsigned set_window(unsigned x, unsigned y, unsigned z);
unsigned moto_emi_set_window(unsigned x, unsigned y, unsigned z) {
  offset_writes++;
  return set_window(x,y,z);
}
unsigned moto_emi_call(unsigned op, unsigned x, unsigned y, unsigned z) {
  if (op == EMI_READ) {
    if (primary_unavailable) return ~0u;
    assert(y == 25 || y == 29);
    struct entry *e = &slots[y];
    return x == 0 ? e->sa : x == 1 ? e->ea : e->apc[z];
  }
  assert(op == EMI_SET && !primary_unavailable);
  return set_window(x,y,z);
}
static unsigned set_window(unsigned x,unsigned y,unsigned z) {
  assert(x >> 24 == 25);
  writes++;
  if (deny)
    return ~0u;
  struct entry *e = &slots[x >> 24];
  assert(!(e->apc[0] >> 31));
  e->sa = (x & 0xffff) - 0x4000;
  e->ea = (y & 0xffff) - 0x4000;
  e->apc[y >> 24] = z;
  return 0;
}
int main(void) {
  slots[29].sa = 0x324;
  assert(radio_windows_request(1, WIFI_AUTO) == 0x11 && writes == 0);
  assert(radio_window_raw[1][0] == 0x324);
  slots[25].apc[0] = ~0u;
  assert(radio_windows_request(1, WIFI_AUTO) == 0x10 && writes == 0);
  assert(radio_window_raw[0][2] == ~0u && radio_window_raw[1][0] == 0x324);
  memset(slots, 0, sizeof slots);
  memset(slots, 0xff, sizeof slots);
  assert(radio_windows_request(1, WIFI_AUTO) == 0x42 && writes == 0);
  memset(slots, 0, sizeof slots);
  assert(radio_windows_request(0, WIFI_AUTO) == 0x50 && writes == 0);
  const struct entry tee = {0x3fc0, 0x3fdf, {0xb69b69, 0xb69d6d}};
  slots[29] = tee;
  deny = 1;
  assert(radio_windows_request(1, WIFI_AUTO) == 0x20 && writes == 1);
  deny = 0;
  writes = 0;
  assert(radio_windows_request(1, WIFI_AUTO) == 1 && writes == 2);
  assert(slots[25].sa == 0x200 && slots[25].ea == 0x26f);
  primary_unavailable = offset_available = 1;
  assert(radio_windows_request(0,WIFI_PRIMARY) == 0x40 && writes == 2);
  assert(radio_windows_request(0,WIFI_OFFSET) == 1 && writes == 2);
  slots[29].sa = slots[29].ea = 0x245;
  assert(radio_windows_request(1, WIFI_AUTO) == 0x11 && writes == 2);
  slots[29] = tee;
  memset(&slots[25], 0, sizeof slots[25]);
  assert(radio_windows_request(1, WIFI_AUTO) == 1 && offset_writes == 2);
  assert(radio_windows_request(1, WIFI_AUTO) == 1 && offset_writes == 2);
  for (int d = 0; d < 16; d++) {
    unsigned actual = (slots[25].apc[d / 8] >> ((d % 8) * 3)) & 7;
    assert(actual == (d == 0 || d == 2 ? 0u : 5u));
  }
  memset(&slots[25], 0, sizeof slots[25]);
  stale = 1;
  assert(radio_windows_request(1, WIFI_AUTO) == 0x20 && offset_writes == 3);
  assert(slots[25].apc[0] == 0);
  assert(memcmp(&slots[29], &tee, sizeof tee) == 0);
}
