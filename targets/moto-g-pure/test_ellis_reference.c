// The reference for test_ellis_smc.c: windows.h calling the secure monitor
// directly, as it did when it ran inside the kernel.
#define moto_emi_call monitor_call
#define moto_emi_read_offset monitor_read_offset
#define moto_emi_set_window monitor_set_window
#include "../../platform/sel4/board/wifi/windows.h"

unsigned reference_request(int prepare, unsigned reader, unsigned raw[8]) {
  unsigned result = radio_windows_request(prepare, reader);
  for (unsigned i = 0; i < 8; ++i) raw[i] = radio_window_raw[i / 4][i % 4];
  return result;
}
unsigned reference_permissions(unsigned group) { return radio_permissions(group); }
