// One connectivity window contains both dedicated radio buffers.
// Region 29 is read only and belongs to secure firmware on Ellis.
// Firmware reads these registers in units of 64 KiB relative to DRAM base,
// and programs the start and end as absolute physical page numbers.
#include <stdint.h>
extern unsigned moto_emi_call(unsigned operation, unsigned x, unsigned y,
                              unsigned z);
extern unsigned moto_emi_read_offset(unsigned offset);
extern unsigned moto_emi_set_window(unsigned start, unsigned end, unsigned permissions);
enum { EMI_SET = 0, EMI_READ = 2 };
struct radio_window {
  unsigned region, start, size;
};
static const struct radio_window connectivity = {25, 0x42000000, 0x700000};
static unsigned radio_permissions(unsigned group) {
  unsigned p = 0;
  for (unsigned d = 0; d < 8; ++d)
    if (group || (d != 0 && d != 2))
      p |= 5u << (3 * d);
  return p;
}
static unsigned radio_window_raw[2][4];
static void read_region(unsigned region, unsigned *raw, int by_offset) {
  for (unsigned i = 0; i < 4; i++) {
    raw[i] = by_offset ? moto_emi_read_offset((i + 1) * 0x100 + region * 4)
                       : moto_emi_call(EMI_READ, i < 2 ? i : 2, region, i == 3);
  }
}
static unsigned window_state(void) {
  const struct radio_window *w = &connectivity;
  const unsigned *raw = radio_window_raw[0];
  unsigned start = raw[0], end = raw[1], low = raw[2], high = raw[3];
  if ((start & end & low & high) == UINT32_MAX)
    return 3; // All four secure-monitor reads returned -1.
  if (!(start | end | low | high))
    return 0;
  if (start == (w->start - 0x40000000u) >> 16 &&
      end == (w->start + w->size - 1 - 0x40000000u) >> 16 &&
      (low & 0x7fffffffu) == radio_permissions(0) &&
      high == radio_permissions(1))
    return 1;
  return 2;
}
enum { WIFI_AUTO, WIFI_PRIMARY, WIFI_OFFSET };
static unsigned radio_windows_request(int prepare, unsigned reader) {
  const struct radio_window *w = &connectivity;
  int by_offset = reader == WIFI_OFFSET;
  read_region(w->region, radio_window_raw[0], by_offset);
  unsigned state = window_state();
  if (reader == WIFI_AUTO && state == 3) {
    by_offset = 1;
    read_region(w->region, radio_window_raw[0], by_offset);
    state = window_state();
  }
  unsigned *secure = radio_window_raw[1];
  read_region(29, secure, by_offset);
  if (state == 3) return by_offset ? 0x42 : 0x40;
  if (state == 2) return 0x10;
  const unsigned first = (w->start - 0x40000000u) >> 16;
  const unsigned last = first + (w->size >> 16) - 1;
  // Reject malformed or overlapping secure reservations before either write.
  if (secure[0] > secure[1] || secure[1] > 0xbfff ||
      secure[2] == UINT32_MAX || secure[3] == UINT32_MAX ||
      (secure[0] <= last && secure[1] >= first)) return 0x11;
  if (state == 1) return 1;
  if (!prepare) return 0x50;
  unsigned start = (w->region << 24) | (w->start >> 16);
  unsigned end = (w->start + w->size - 1) >> 16;
  unsigned high = radio_permissions(1);
  unsigned low = radio_permissions(0) | (1u << 31);
  unsigned upper_end = end | (1u << 24);
  unsigned result = by_offset ? moto_emi_set_window(start, upper_end, high)
                              : moto_emi_call(EMI_SET, start, upper_end, high);
  if (result) return 0x20;
  read_region(w->region, radio_window_raw[0], by_offset);
  unsigned *raw = radio_window_raw[0];
  if (raw[0] != first || raw[1] != last || raw[2] != 0 || raw[3] != high)
    return 0x20;
  result = by_offset ? moto_emi_set_window(start, end, low)
                     : moto_emi_call(EMI_SET, start, end, low);
  if (result) return 0x20;
  read_region(w->region, raw, by_offset);
  return window_state() == 1 ? 1 : 0x20;
}
