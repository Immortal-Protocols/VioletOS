// WPA2-Personal CCMP glue. hostap owns replay checks, MIC verification,
// key unwrapping, retransmissions and key-install state. With management
// frame protection (802.11w) it also unwraps the IGTK; Radio checks BIP.
#include "utils/includes.h"
#include "utils/common.h"

#include "crypto/crypto.h"
#include "rsn_supp/wpa.h"
#include "utils/eloop.h"

extern int radio_random(u8 *out, size_t n);
extern int radio_send_eapol(void *, const u8 *, size_t);
extern int radio_set_ccmp(void *, int, int, const u8 *, const u8 *, size_t);
extern int radio_set_igtk(void *, int, const u8 *, const u8 *);
extern unsigned ticks(void), ticks_per_ms;

static struct {
  struct wpa_sm *sm;
  enum wpa_states state;
  void *radio;
  u8 bssid[6];
  bool failed;
  bool mfp;
} station_link;

static void state(void *ctx, enum wpa_states s) { station_link.state = s; }
static enum wpa_states current(void *ctx) { return station_link.state; }
static void fail(void *ctx, u16 reason) { station_link.failed = true; }
static void reconnect(void *ctx) { station_link.failed = true; }
static void *network(void *ctx) { return station_link.radio; }
static int bssid(void *ctx, u8 *out) {
  memcpy(out, station_link.bssid, 6);
  return 0;
}
static int beacon(void *ctx) {
  return 0;
} // AP's complete RSNE is supplied on open.
static void cancel(void *ctx) {}
// This profile does not use a firmware PMKID cache.
static int add_pmkid(void *ctx, void *net, const u8 *ap, const u8 *id,
                     const u8 *fils, const u8 *pmk, size_t len, u32 ttl,
                     u8 threshold, int akmp) {
  return -1;
}
static int remove_pmkid(void *ctx, void *net, const u8 *ap, const u8 *id,
                        const u8 *fils) {
  return -1;
}
static int protect(void *ctx, const u8 *addr, int protection, int type) {
  return 0;
}
static int send_eapol(void *ctx, const u8 *dest, u16 proto, const u8 *buf,
                      size_t n) {
  if (proto != 0x888e || memcmp(dest, station_link.bssid, 6))
    return -1;
  return radio_send_eapol(station_link.radio, buf, n);
}
static int install(void *ctx, int lid, enum wpa_alg alg, const u8 *peer,
                   int key_id, int tx, const u8 *seq, size_t sn, const u8 *key,
                   size_t n, enum key_flag flags) {
  if (alg == WPA_ALG_BIP_CMAC_128) {
    // The integrity group key: ID 4 or 5, receive only, with its packet number.
    if (lid != -1 || !station_link.mfp || n != 16 || sn != 6 || !seq ||
        key_id < 4 || key_id > 5 || tx || !peer ||
        !is_broadcast_ether_addr(peer) || (flags & KEY_FLAG_PAIRWISE))
      return -1;
    return radio_set_igtk(station_link.radio, key_id, key, seq);
  }
  if (lid != -1 || alg != WPA_ALG_CCMP || n != 16 || key_id < 0 || key_id > 3 ||
      sn > 8 || !peer)
    return -1;
  bool pair = flags & KEY_FLAG_PAIRWISE;
  if ((pair && memcmp(peer, station_link.bssid, 6)) ||
      (!pair && !is_broadcast_ether_addr(peer)))
    return -1;
  return radio_set_ccmp(station_link.radio, pair, key_id, key, seq, sn);
}
static u8 *alloc_frame(void *ctx, u8 type, const void *data, u16 n,
                       size_t *size, void **body) {
  *size = 4u + n;
  u8 *frame = os_zalloc(*size);
  if (!frame)
    return NULL;
  frame[0] = 2;
  frame[1] = type;
  WPA_PUT_BE16(frame + 2, n);
  if (body)
    *body = frame + 4;
  if (data)
    memcpy(frame + 4, data, n);
  return frame;
}

void station_wpa_close(void) {
  if (station_link.sm)
    wpa_sm_deinit(station_link.sm);
  memset(&station_link, 0, sizeof(station_link));
}
int station_wpa_open(void *radio, const u8 *mac, const u8 *ap, const u8 *ssid,
                     size_t ssid_len, const u8 *ap_ie, size_t ie_len,
                     const u8 *assoc_ie, size_t assoc_len, const u8 *pmk,
                     int akm, int mfp) {
  station_wpa_close();
  // Key management 00-0F-AC:2 (PSK) or :6 (PSK with SHA-256); management
  // frame protection off (0), negotiated (1) or required (2).
  if ((akm != 2 && akm != 6) || mfp < 0 || mfp > 2)
    return -1;
  struct wpa_sm_ctx *cb = os_zalloc(sizeof(*cb));
  if (!cb)
    return -1;
  cb->ctx = &station_link;
  cb->set_state = state;
  cb->get_state = current;
  cb->deauthenticate = fail;
  cb->reconnect = reconnect;
  cb->set_key = install;
  cb->get_network_ctx = network;
  cb->get_bssid = bssid;
  cb->ether_send = send_eapol;
  cb->get_beacon_ie = beacon;
  cb->cancel_auth_timeout = cancel;
  cb->alloc_eapol = alloc_frame;
  cb->mlme_setprotection = protect;
  cb->add_pmkid = add_pmkid;
  cb->remove_pmkid = remove_pmkid;
  station_link.sm = wpa_sm_init(cb);
  if (!station_link.sm) {
    free(cb);
    return -1;
  }
  station_link.radio = radio;
  station_link.mfp = mfp != 0;
  memcpy(station_link.bssid, ap, 6);
  struct rsn_supp_config conf = {0};
  conf.network_ctx = radio;
  conf.allowed_pairwise_cipher = WPA_CIPHER_CCMP;
  conf.ssid = ssid;
  conf.ssid_len = ssid_len;
  wpa_sm_set_config(station_link.sm, &conf);
  wpa_sm_set_param(station_link.sm, WPA_PARAM_PROTO, WPA_PROTO_RSN);
  wpa_sm_set_param(station_link.sm, WPA_PARAM_PAIRWISE, WPA_CIPHER_CCMP);
  wpa_sm_set_param(station_link.sm, WPA_PARAM_GROUP, WPA_CIPHER_CCMP);
  wpa_sm_set_param(station_link.sm, WPA_PARAM_KEY_MGMT,
                   akm == 6 ? WPA_KEY_MGMT_PSK_SHA256 : WPA_KEY_MGMT_PSK);
  wpa_sm_set_param(station_link.sm, WPA_PARAM_MFP, mfp);
  wpa_sm_set_param(station_link.sm, WPA_PARAM_MGMT_GROUP,
                   mfp ? WPA_CIPHER_AES_128_CMAC : 0);
  wpa_sm_set_param(station_link.sm, WPA_PARAM_RSN_ENABLED, 1);
  wpa_sm_set_own_addr(station_link.sm, mac);
  wpa_sm_set_pmk(station_link.sm, pmk, 32, NULL, NULL);
  wpa_sm_notify_assoc(station_link.sm, ap);
  if (wpa_sm_set_ap_rsn_ie(station_link.sm, ap_ie, ie_len) ||
      wpa_sm_set_assoc_wpa_ie(station_link.sm, assoc_ie, assoc_len)) {
    station_wpa_close();
    return -1;
  }
  station_link.state = WPA_ASSOCIATED;
  return 0;
}
// 1: the handshake is complete. 0: nothing to report. -2: hostap rejected
// this frame (replayed counter, wrong MIC, malformed...) and the link is as
// it was. -1: hostap gave the link up (its deauthenticate or reconnect
// callback ran, now or earlier).
int station_wpa_rx(const u8 *eapol, size_t n) {
  if (!station_link.sm || station_link.failed)
    return -1;
  int rc = wpa_sm_rx_eapol(station_link.sm, station_link.bssid, eapol, n,
                           FRAME_ENCRYPTION_UNKNOWN);
  if (station_link.failed)
    return -1;
  return rc < 0 ? -2 : (station_link.state == WPA_COMPLETED);
}
