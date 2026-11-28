/* The TLS session under EAP-TLS and PEAP (eap.rs `Tls`), on BearSSL.
 *
 * One TLS 1.2 client whose records travel inside EAP packets instead of a
 * socket. The server is authenticated against ONE certificate authority the
 * network's profile names and a server name it must present; there is no
 * "accept any certificate" mode, because a supplicant that skips this check
 * hands the user's password hash to any access point with the right name.
 *
 * No allocation: the caller supplies eap_tls_size() bytes. Not linked into
 * Radio yet (targets/moto-g-pure/README.md, "Enterprise Wi-Fi"); the host
 * test against hostap's EAP server runs exactly this file.
 */
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include "bearssl.h"

#define DN_MAX 512
#define KEY_MAX 520
#define OUT_MAX 8192
#define APP_MAX 2048
#define REC_MAX 16

struct eap_tls {
	br_ssl_client_context sc;
	br_x509_minimal_context xc;
	br_x509_trust_anchor anchor;
	br_x509_certificate chain[1];
	br_skey_decoder_context key;
	unsigned char dn[DN_MAX];
	size_t dn_len;
	int dn_overflow;
	unsigned char anchor_key[KEY_MAX];
	unsigned char out[OUT_MAX];
	size_t out_len;
	unsigned char app[APP_MAX];
	size_t app_len;
	/* Where each received record's plaintext ends in app[]: eap_tls_read
	 * hands over one record at a time, never two run together. */
	size_t rec_end[REC_MAX];
	size_t recs;
	int up;
	int dead;
	unsigned char io[BR_SSL_BUFSIZE_BIDI];
};

size_t eap_tls_size(void)
{
	return sizeof(struct eap_tls);
}

static void dn_append(void *ctx, const void *buf, size_t len)
{
	struct eap_tls *t = ctx;

	if (len > DN_MAX - t->dn_len) {
		t->dn_overflow = 1;
		return;
	}
	memcpy(t->dn + t->dn_len, buf, len);
	t->dn_len += len;
}

/* The authority's certificate as BearSSL's trust anchor (its name and key). */
static int anchor_from(struct eap_tls *t, const unsigned char *der, size_t len)
{
	br_x509_decoder_context dc;
	br_x509_pkey *pk;

	br_x509_decoder_init(&dc, dn_append, t);
	br_x509_decoder_push(&dc, der, len);
	pk = br_x509_decoder_get_pkey(&dc);
	if (!pk || br_x509_decoder_last_error(&dc) != 0 || t->dn_overflow || !br_x509_decoder_isCA(&dc))
		return -1;
	t->anchor.dn.data = t->dn;
	t->anchor.dn.len = t->dn_len;
	t->anchor.flags = BR_X509_TA_CA;
	t->anchor.pkey.key_type = pk->key_type;
	if (pk->key_type == BR_KEYTYPE_RSA) {
		if (pk->key.rsa.nlen + pk->key.rsa.elen > KEY_MAX)
			return -1;
		memcpy(t->anchor_key, pk->key.rsa.n, pk->key.rsa.nlen);
		memcpy(t->anchor_key + pk->key.rsa.nlen, pk->key.rsa.e, pk->key.rsa.elen);
		t->anchor.pkey.key.rsa.n = t->anchor_key;
		t->anchor.pkey.key.rsa.nlen = pk->key.rsa.nlen;
		t->anchor.pkey.key.rsa.e = t->anchor_key + pk->key.rsa.nlen;
		t->anchor.pkey.key.rsa.elen = pk->key.rsa.elen;
	} else if (pk->key_type == BR_KEYTYPE_EC) {
		if (pk->key.ec.qlen > KEY_MAX)
			return -1;
		memcpy(t->anchor_key, pk->key.ec.q, pk->key.ec.qlen);
		t->anchor.pkey.key.ec.curve = pk->key.ec.curve;
		t->anchor.pkey.key.ec.q = t->anchor_key;
		t->anchor.pkey.key.ec.qlen = pk->key.ec.qlen;
	} else {
		return -1;
	}
	return 0;
}

/* Move whatever the engine has ready into our two queues. */
static void drain(struct eap_tls *t)
{
	br_ssl_engine_context *e = &t->sc.eng;

	for (;;) {
		unsigned st = br_ssl_engine_current_state(e);
		unsigned char *b;
		size_t n;

		if (st == BR_SSL_CLOSED) {
			t->dead = 1;
			return;
		}
		if (st & BR_SSL_SENDAPP)
			t->up = 1;
		if (st & BR_SSL_SENDREC) {
			b = br_ssl_engine_sendrec_buf(e, &n);
			if (n > OUT_MAX - t->out_len) {
				t->dead = 1;
				return;
			}
			memcpy(t->out + t->out_len, b, n);
			t->out_len += n;
			br_ssl_engine_sendrec_ack(e, n);
			continue;
		}
		if (st & BR_SSL_RECVAPP) {
			b = br_ssl_engine_recvapp_buf(e, &n);
			if (n > APP_MAX - t->app_len || t->recs == REC_MAX) {
				t->dead = 1;
				return;
			}
			memcpy(t->app + t->app_len, b, n);
			t->app_len += n;
			t->rec_end[t->recs++] = t->app_len;
			br_ssl_engine_recvapp_ack(e, n);
			continue;
		}
		return;
	}
}

/* Start a session in `mem` (eap_tls_size() bytes, suitably aligned).
 * ca: the authority's certificate (DER). name: what the server's certificate
 * must say (a DNS name). cert/key: the client's certificate and private key
 * (DER; EC P-256 or RSA) for EAP-TLS, or NULL for PEAP. entropy: at least 32
 * fresh random bytes. days/seconds: now, as BearSSL counts it (days since
 * 0 AD, seconds in the day), for the certificates' validity dates.
 * 0, or -1 with nothing usable left in `mem`. */
int eap_tls_open(void *mem, const unsigned char *ca, size_t ca_len, const char *name,
		 const unsigned char *cert, size_t cert_len, const unsigned char *key, size_t key_len,
		 const unsigned char *entropy, size_t entropy_len, uint32_t days, uint32_t seconds)
{
	struct eap_tls *t = mem;

	memset(t, 0, sizeof(*t));
	t->dead = 1;
	if (!ca || !name || !name[0] || !entropy || entropy_len < 32 || days == 0)
		return -1;
	if (anchor_from(t, ca, ca_len) < 0)
		return -1;
	br_ssl_client_init_full(&t->sc, &t->xc, &t->anchor, 1);
	br_ssl_engine_set_versions(&t->sc.eng, BR_TLS12, BR_TLS12);
	br_x509_minimal_set_time(&t->xc, days, seconds);
	br_ssl_engine_set_buffer(&t->sc.eng, t->io, sizeof(t->io), 1);
	br_ssl_engine_inject_entropy(&t->sc.eng, entropy, entropy_len);
	if (cert) {
		int kind;

		if (!key)
			return -1;
		br_skey_decoder_init(&t->key);
		br_skey_decoder_push(&t->key, key, key_len);
		if (br_skey_decoder_last_error(&t->key) != 0)
			return -1;
		t->chain[0].data = (unsigned char *) cert;
		t->chain[0].data_len = cert_len;
		kind = br_skey_decoder_key_type(&t->key);
		if (kind == BR_KEYTYPE_EC)
			br_ssl_client_set_single_ec(&t->sc, t->chain, 1, br_skey_decoder_get_ec(&t->key),
						    BR_KEYTYPE_SIGN, 0, br_ec_get_default(),
						    br_ecdsa_sign_asn1_get_default());
		else if (kind == BR_KEYTYPE_RSA)
			br_ssl_client_set_single_rsa(&t->sc, t->chain, 1, br_skey_decoder_get_rsa(&t->key),
						     br_rsa_pkcs1_sign_get_default());
		else
			return -1;
	}
	if (!br_ssl_client_reset(&t->sc, name, 0))
		return -1;
	t->dead = 0;
	drain(t);
	return t->dead ? -1 : 0;
}

/* 1, or 0 once the session is dead. */
int eap_tls_input(void *mem, const unsigned char *data, size_t len)
{
	struct eap_tls *t = mem;
	br_ssl_engine_context *e = &t->sc.eng;

	while (len && !t->dead) {
		unsigned char *b;
		size_t n;

		drain(t);
		if (t->dead)
			break;
		if (!(br_ssl_engine_current_state(e) & BR_SSL_RECVREC)) {
			/* Bytes the engine will not take: not a TLS stream we follow. */
			t->dead = 1;
			break;
		}
		b = br_ssl_engine_recvrec_buf(e, &n);
		if (n > len)
			n = len;
		memcpy(b, data, n);
		br_ssl_engine_recvrec_ack(e, n);
		data += n;
		len -= n;
	}
	if (!t->dead)
		drain(t);
	return !t->dead;
}

size_t eap_tls_output(void *mem, unsigned char *out, size_t max)
{
	struct eap_tls *t = mem;
	size_t n = t->out_len < max ? t->out_len : max;

	memcpy(out, t->out, n);
	memmove(t->out, t->out + n, t->out_len - n);
	t->out_len -= n;
	return n;
}

int eap_tls_established(void *mem)
{
	struct eap_tls *t = mem;

	return t->up && !t->dead;
}

/* BearSSL's last error (BR_ERR_*), for the log. */
int eap_tls_error(void *mem)
{
	struct eap_tls *t = mem;

	return br_ssl_engine_last_error(&t->sc.eng);
}

int eap_tls_export(void *mem, const unsigned char *label, size_t label_len, unsigned char *out, size_t len)
{
	struct eap_tls *t = mem;
	char text[64];

	if (!eap_tls_established(t) || label_len >= sizeof(text))
		return 0;
	memcpy(text, label, label_len);
	text[label_len] = 0;
	return br_ssl_key_export(&t->sc.eng, out, len, text, NULL, 0);
}

int eap_tls_write(void *mem, const unsigned char *data, size_t len)
{
	struct eap_tls *t = mem;
	br_ssl_engine_context *e = &t->sc.eng;
	unsigned char *b;
	size_t n;

	if (!eap_tls_established(t) || !(br_ssl_engine_current_state(e) & BR_SSL_SENDAPP))
		return 0;
	b = br_ssl_engine_sendapp_buf(e, &n);
	if (len > n)
		return 0;
	memcpy(b, data, len);
	br_ssl_engine_sendapp_ack(e, len);
	br_ssl_engine_flush(e, 0);
	drain(t);
	return !t->dead;
}

size_t eap_tls_read(void *mem, unsigned char *out, size_t max)
{
	struct eap_tls *t = mem;
	size_t n = t->recs ? t->rec_end[0] : 0;
	size_t i;

	/* The first record's plaintext (all of it that fits in `out`). */
	if (n > max)
		n = max;
	memcpy(out, t->app, n);
	memmove(t->app, t->app + n, t->app_len - n);
	t->app_len -= n;
	for (i = 0; i < t->recs; i++)
		t->rec_end[i] -= n;
	if (t->recs && t->rec_end[0] == 0) {
		memmove(t->rec_end, t->rec_end + 1, (t->recs - 1) * sizeof t->rec_end[0]);
		t->recs--;
	}
	return n;
}
