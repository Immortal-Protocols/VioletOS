// Host-only callbacks for exercising the production station state machine.
#include "utils/includes.h"

#include "utils/common.h"
#include "utils/eloop.h"
#include <stddef.h>
#include <string.h>

static unsigned char last_frame[2048], installed[2][16];
static size_t frame_size;
static unsigned installs[2];
static unsigned char igtk[2][16], igtk_ipn[2][6];
static unsigned igtk_installs[2];

int radio_send_eapol(void *ctx, const unsigned char *p, size_t len) {
  if (!ctx || len > sizeof(last_frame))
    return -1;
  memcpy(last_frame, p, len);
  frame_size = len;
  return 0;
}
int radio_set_ccmp(void *ctx, int pair, int id, const unsigned char *key,
                   const unsigned char *seq, size_t len) {
  if (!ctx || (pair ? id != 0 : (id < 1 || id > 3)))
    return -1;
  memcpy(installed[pair], key, 16);
  installs[pair]++;
  return 0;
}
int radio_set_igtk(void *ctx, int id, const unsigned char *key,
                   const unsigned char *ipn) {
  if (!ctx || id < 4 || id > 5)
    return -1;
  memcpy(igtk[id - 4], key, 16);
  memcpy(igtk_ipn[id - 4], ipn, 6);
  igtk_installs[id - 4]++;
  return 0;
}
// The IGTK with key ID `id` (16 bytes) and its packet number (6): install count.
int observed_igtk(int id, unsigned char *out) {
  memcpy(out, igtk[id - 4], 16);
  memcpy(out + 16, igtk_ipn[id - 4], 6);
  return igtk_installs[id - 4];
}
void reset_observed(void) {
  memset(installed, 0, sizeof(installed));
  memset(installs, 0, sizeof(installs));
  memset(igtk, 0, sizeof(igtk));
  memset(igtk_ipn, 0, sizeof(igtk_ipn));
  memset(igtk_installs, 0, sizeof(igtk_installs));
}
int observed_key(int pair, unsigned char *out) {
  memcpy(out, installed[pair], 16);
  return installs[pair];
}
size_t sent_frame(unsigned char *out) {
  memcpy(out, last_frame, frame_size);
  return frame_size;
}
