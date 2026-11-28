/* The access point's side of SAE for test_sae.rs (`make test-sae`): hostap
 * 2.11's own common/sae.c on OpenSSL, and hostapd's anti-clogging tokens
 * (ap/comeback_token.c). Nothing here is VioletOS code under test. */
#include "includes.h"
#include "common.h"
#include "crypto/crypto.h"
#include "common/sae.h"
#include "common/wpa_common.h"
#include "ap/comeback_token.h"

/* As hostapd.h has it (that header needs most of hostapd). */
#define COMEBACK_KEY_SIZE 8

/* sae.c names these only in debug output and for SAE-EXT-KEY, which the
 * tests do not use; wpa_common.c would bring most of hostap with it. */
u32 wpa_akm_to_suite(int akm) { (void) akm; return 0; }
const char *wpa_key_mgmt_txt(int key_mgmt, int proto) { (void) key_mgmt; (void) proto; return ""; }

struct ref {
	struct sae_data sae;
	struct sae_pt *pt;
};

static int groups[] = { 19, 0 };

void *ref_new(int h2e, const u8 *own, const u8 *peer, const u8 *pw, size_t pw_len,
	      const u8 *ssid, size_t ssid_len)
{
	struct ref *r = os_zalloc(sizeof(*r));

	if (!r || sae_set_group(&r->sae, 19) < 0)
		return NULL;
	if (h2e) {
		r->pt = sae_derive_pt(groups, ssid, ssid_len, pw, pw_len, NULL);
		if (!r->pt || sae_prepare_commit_pt(&r->sae, r->pt, own, peer, NULL, NULL) < 0)
			return NULL;
	} else if (sae_prepare_commit(own, peer, pw, pw_len, &r->sae) < 0) {
		return NULL;
	}
	return r;
}

static size_t out(struct wpabuf *b, u8 *dst)
{
	size_t n = b ? wpabuf_len(b) : 0;

	if (n)
		os_memcpy(dst, wpabuf_head(b), n);
	wpabuf_free(b);
	return n;
}

size_t ref_commit(struct ref *r, const u8 *token, size_t token_len, u8 *dst)
{
	struct wpabuf *b = wpabuf_alloc(512);
	struct wpabuf *t = token ? wpabuf_alloc_copy(token, token_len) : NULL;
	int rc = sae_write_commit(&r->sae, b, t, NULL);

	wpabuf_free(t);
	if (rc < 0) {
		wpabuf_free(b);
		return 0;
	}
	return out(b, dst);
}

int ref_parse_commit(struct ref *r, const u8 *data, size_t len, u8 *token, size_t *token_len)
{
	const u8 *t = NULL;
	size_t tl = 0;
	u16 status = sae_parse_commit(&r->sae, data, len, &t, &tl, groups, r->sae.h2e, NULL);

	*token_len = 0;
	if (t && tl <= 256) {
		os_memcpy(token, t, tl);
		*token_len = tl;
	}
	return status;
}

int ref_process(struct ref *r)
{
	return sae_process_commit(&r->sae);
}

size_t ref_confirm(struct ref *r, u8 *dst)
{
	struct wpabuf *b = wpabuf_alloc(128);

	if (sae_write_confirm(&r->sae, b) < 0) {
		wpabuf_free(b);
		return 0;
	}
	return out(b, dst);
}

int ref_check_confirm(struct ref *r, const u8 *data, size_t len)
{
	return sae_check_confirm(&r->sae, data, len, NULL);
}

void ref_keys(struct ref *r, u8 *pmk, u8 *pmkid)
{
	os_memcpy(pmk, r->sae.pmk, 32);
	os_memcpy(pmkid, r->sae.pmkid, 16);
}

void ref_free(struct ref *r)
{
	sae_clear_data(&r->sae);
	sae_deinit_pt(r->pt);
	os_free(r);
}

/* One access point's token state, as hostapd keeps it. */
static struct os_reltime last_key;
static u8 key[COMEBACK_KEY_SIZE];
static u16 pending[256];
static u16 next_idx;

size_t ref_token_request(int h2e, const u8 *sta, u8 *dst)
{
	return out(auth_build_token_req(&last_key, key, ++next_idx, pending, sizeof(pending) / sizeof(pending[0]),
					19, sta, h2e), dst);
}

int ref_token_check(const u8 *sta, const u8 *token, size_t len)
{
	return check_comeback_token(key, pending, sta, token, len);
}
