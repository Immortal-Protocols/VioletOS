// Host-only callbacks for exercising the production station state machine.
#include "utils/includes.h"

#include "utils/common.h"
#include "utils/eloop.h"
#include <stddef.h>
#include <string.h>

static unsigned char last_frame[2048], installed[2][16];
static size_t frame_size;
static unsigned installs[2];

int radio_send_eapol(void *ctx, const unsigned char *p, size_t len) {
  if (!ctx || len > sizeof(last_frame))
    return -1;
  memcpy(last_frame, p, len);
  frame_size = len;
  return 0;
}
int radio_set_ccmp(void *ctx, int pair, int id, const unsigned char *key,
                   const unsigned char *seq, size_t len) {
  if (!ctx || (pair ? id != 0 : id != 1))
    return -1;
  memcpy(installed[pair], key, 16);
  installs[pair]++;
  return 0;
}
int observed_key(int pair, unsigned char *out) {
  memcpy(out, installed[pair], 16);
  return installs[pair];
}
size_t sent_frame(unsigned char *out) {
  memcpy(out, last_frame, frame_size);
  return frame_size;
}
