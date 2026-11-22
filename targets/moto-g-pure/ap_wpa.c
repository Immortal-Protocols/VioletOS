// WPA2-PSK (CCMP) authenticator for the VioletOS access point: one phone,
// one 4-way handshake. hostap supplies PTK derivation, MICs and AES key wrap.
#include "utils/includes.h"
#include "utils/common.h"

#include "common/defs.h"
#include "common/wpa_common.h"
#include "crypto/aes_wrap.h"

extern int radio_random(u8 *out, size_t n);
extern int radio_send_eapol(void *, const u8 *, size_t);
extern int radio_set_ccmp(void *, int, int, const u8 *, const u8 *, size_t);
extern unsigned ticks(void), ticks_per_ms;

enum { IDLE, SENT_1, SENT_3, DONE, FAILED };

static struct {
  void *radio;
  int state, sends, received, mic_failures;
  unsigned last;
  u8 aa[6], spa[6], pmk[32], anonce[32], gtk[16];
  u64 replay;
  struct wpa_ptk ptk;
} ap;

static const u8 rsn_ie[] = {48, 20, 1, 0, 0, 15, 172, 4, 1, 0, 0, 15, 172, 4,
                            1, 0, 0, 15, 172, 2, 0, 0};

// EAPOL header (4) + key descriptor (95) + key data.
static int send_key(u16 info, const u8 *data, size_t data_len) {
  u8 f[4 + 95 + 64];
  if (data_len > 64)
    return -1;
  memset(f, 0, sizeof(f));
  size_t body = 95 + data_len;
  f[0] = 2;
  f[1] = 3;
  WPA_PUT_BE16(f + 2, body);
  u8 *k = f + 4;
  k[0] = 2; // RSN key descriptor
  WPA_PUT_BE16(k + 1, info);
  WPA_PUT_BE16(k + 3, 16);
  WPA_PUT_BE64(k + 5, ap.replay);
  memcpy(k + 13, ap.anonce, 32);
  WPA_PUT_BE16(k + 93, data_len);
  memcpy(k + 95, data, data_len);
  if (info & WPA_KEY_INFO_MIC &&
      wpa_eapol_key_mic(ap.ptk.kck, ap.ptk.kck_len, WPA_KEY_MGMT_PSK, 2, f,
                        4 + body, k + 77))
    return -1;
  ap.last = ticks();
  ap.sends++;
  return radio_send_eapol(ap.radio, f, 4 + body);
}

static int send_1(void) {
  return send_key(WPA_KEY_INFO_TYPE_HMAC_SHA1_AES | WPA_KEY_INFO_KEY_TYPE |
                      WPA_KEY_INFO_ACK,
                  NULL, 0);
}

static int send_3(void) {
  // RSN IE, then the GTK KDE (key ID 1), padded to a multiple of 8 for AES wrap.
  u8 plain[56], wrapped[64];
  size_t n = 0;
  memcpy(plain, rsn_ie, sizeof(rsn_ie));
  n += sizeof(rsn_ie);
  const u8 kde[] = {0xdd, 22, 0x00, 0x0f, 0xac, 0x01, 0x01, 0x00};
  memcpy(plain + n, kde, sizeof(kde));
  n += sizeof(kde);
  memcpy(plain + n, ap.gtk, 16);
  n += 16;
  if (n % 8) {
    plain[n++] = 0xdd;
    while (n % 8)
      plain[n++] = 0;
  }
  if (aes_wrap(ap.ptk.kek, ap.ptk.kek_len, n / 8, plain, wrapped))
    return -1;
  return send_key(WPA_KEY_INFO_TYPE_HMAC_SHA1_AES | WPA_KEY_INFO_KEY_TYPE |
                      WPA_KEY_INFO_INSTALL | WPA_KEY_INFO_ACK |
                      WPA_KEY_INFO_MIC | WPA_KEY_INFO_SECURE |
                      WPA_KEY_INFO_ENCR_KEY_DATA,
                  wrapped, n + 8);
}

int ap_wpa_start(void *radio, const u8 *aa, const u8 *spa, const u8 *pmk) {
  memset(&ap, 0, sizeof(ap));
  ap.radio = radio;
  memcpy(ap.aa, aa, 6);
  memcpy(ap.spa, spa, 6);
  memcpy(ap.pmk, pmk, 32);
  if (radio_random(ap.anonce, 32) || radio_random(ap.gtk, 16)) {
    ap.state = FAILED;
    return -1;
  }
  ap.replay = 1;
  ap.state = SENT_1;
  return send_1();
}

static int verify_mic(u8 *f, size_t n) {
  u8 mic[16], expect[16];
  u8 *k = f + 4;
  memcpy(mic, k + 77, 16);
  memset(k + 77, 0, 16);
  int ok = !wpa_eapol_key_mic(ap.ptk.kck, ap.ptk.kck_len, WPA_KEY_MGMT_PSK, 2,
                              f, n, expect) &&
           !os_memcmp_const(mic, expect, 16);
  memcpy(k + 77, mic, 16);
  return ok;
}

// Returns 1 when the pairwise key is installed, 0 to continue, -1 on failure.
int ap_wpa_rx(const u8 *eapol, size_t n) {
  u8 f[4 + 95 + 256];
  if (ap.state == DONE)
    return 1;
  if (ap.state == FAILED || n < 4 + 95 || n > sizeof(f))
    return ap.state == FAILED ? -1 : 0;
  memcpy(f, eapol, n);
  ap.received++;
  u8 *k = f + 4;
  if (f[1] != 3 || k[0] != 2 || WPA_GET_BE64(k + 5) != ap.replay)
    return 0;
  u16 info = WPA_GET_BE16(k + 1);
  if (!(info & WPA_KEY_INFO_MIC) || !(info & WPA_KEY_INFO_KEY_TYPE))
    return 0;
  if (ap.state == SENT_1) {
    if (wpa_pmk_to_ptk(ap.pmk, 32, "Pairwise key expansion", ap.aa, ap.spa,
                       ap.anonce, k + 13, &ap.ptk, WPA_KEY_MGMT_PSK,
                       WPA_CIPHER_CCMP, NULL, 0, 0))
      return -1;
    if (!verify_mic(f, n)) {
      ap.mic_failures++;
      return 0; // wrong passphrase or a stale frame: keep waiting
    }
    ap.replay++;
    ap.sends = 0;
    ap.state = SENT_3;
    return send_3() ? -1 : 0;
  }
  if (ap.state == SENT_3 && (info & WPA_KEY_INFO_SECURE) && verify_mic(f, n)) {
    if (radio_set_ccmp(ap.radio, 1, 0, ap.ptk.tk, NULL, 0))
      return -1;
    ap.state = DONE;
    return 1;
  }
  return 0;
}

// Retransmit message 1 or 3 every second, four times at most.
int ap_wpa_poll(void) {
  if (ap.state == DONE)
    return 1;
  if (ap.state == FAILED)
    return -1;
  if (ap.state == IDLE || ticks() - ap.last < 1000 * ticks_per_ms)
    return 0;
  if (ap.sends >= 4) {
    ap.state = FAILED;
    return -1;
  }
  return (ap.state == SENT_1 ? send_1() : send_3()) ? -1 : 0;
}

int ap_wpa_done(void) { return ap.state == DONE; }

// state | sends << 4 | frames received << 8 | MIC failures << 16
unsigned ap_wpa_report(void) {
  return (unsigned)ap.state | (unsigned)ap.sends << 4 | (unsigned)ap.received << 8 |
         (unsigned)ap.mic_failures << 16;
}
