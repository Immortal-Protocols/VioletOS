/* The authentication server's side of EAP for test_eap.rs
 * (`targets/moto-g-pure/test_eap.sh hostap`): hostap 2.11's own EAP server
 * state machine with its EAP-TLS, PEAP and MSCHAPv2 methods on OpenSSL, as
 * hostapd's integrated server runs them. Nothing here is VioletOS code under
 * test. */
#include "includes.h"
#include "common.h"
#include "crypto/tls.h"
#include "eap_server/eap.h"
#include "eap_server/eap_methods.h"

struct ref {
	struct eap_sm *sm;
	struct eap_eapol_interface *io;
	struct eap_config conf;
	void *tls;
	int method;
	char user[64];
	char password[64];
};

static int get_user(void *ctx, const u8 *identity, size_t identity_len, int phase2, struct eap_user *user)
{
	struct ref *r = ctx;

	os_memset(user, 0, sizeof(*user));
	user->force_version = -1;
	user->methods[0].vendor = EAP_VENDOR_IETF;
	if (!phase2) {
		user->methods[0].method = r->method;
		return 0;
	}
	if (identity_len != os_strlen(r->user) || os_memcmp(identity, r->user, identity_len) != 0)
		return -1;
	user->methods[0].method = EAP_TYPE_MSCHAPV2;
	user->password = (u8 *) os_strdup(r->password);
	user->password_len = os_strlen(r->password);
	return user->password ? 0 : -1;
}

static const char *id_text(void *ctx, size_t *len)
{
	*len = 0;
	return NULL;
}

static const struct eapol_callbacks callbacks = {
	.get_eap_user = get_user,
	.get_eap_req_id_text = id_text,
};

static int registered;

/* method: 13 (EAP-TLS, client certificate required) or 25 (PEAP with
 * MSCHAPv2 inside). ca/cert/key: PEM files. fragment: TLS bytes per packet. */
void *ref_new(int method, const char *ca, const char *cert, const char *key, const char *user,
	      const char *password, int fragment)
{
	struct ref *r = os_zalloc(sizeof(*r));
	struct tls_config tconf;
	struct tls_connection_params params;
	struct eap_session_data session;

	if (!r)
		return NULL;
	if (!registered) {
		if (eap_server_identity_register() < 0 || eap_server_tls_register() < 0 ||
		    eap_server_peap_register() < 0 || eap_server_mschapv2_register() < 0)
			return NULL;
		registered = 1;
	}
	r->method = method;
	os_strlcpy(r->user, user, sizeof(r->user));
	os_strlcpy(r->password, password, sizeof(r->password));
	os_memset(&tconf, 0, sizeof(tconf));
	r->tls = tls_init(&tconf);
	if (!r->tls)
		return NULL;
	os_memset(&params, 0, sizeof(params));
	params.ca_cert = ca;
	params.client_cert = cert;
	params.private_key = key;
	if (tls_global_set_params(r->tls, &params) < 0 || tls_global_set_verify(r->tls, 0, 1) < 0)
		return NULL;
	r->conf.ssl_ctx = r->tls;
	r->conf.eap_server = 1;
	r->conf.fragment_size = fragment;
	r->conf.max_auth_rounds = 100;
	r->conf.max_auth_rounds_short = 50;
	os_memset(&session, 0, sizeof(session));
	r->sm = eap_server_sm_init(r, &callbacks, &r->conf, &session);
	if (!r->sm)
		return NULL;
	r->io = eap_get_interface(r->sm);
	r->io->portEnabled = true;
	r->io->eapRestart = true;
	return r;
}

/* Give the server the peer's response (none at the start) and run it.
 * Returns its next packet's length in `out`; *state is 1 after EAP-Success,
 * 2 after EAP-Failure, else 0. */
size_t ref_step(struct ref *r, const u8 *resp, size_t resp_len, u8 *out, size_t max, int *state)
{
	size_t n = 0;
	int turns = 0;

	if (resp) {
		wpabuf_free(r->io->eapRespData);
		r->io->eapRespData = wpabuf_alloc_copy(resp, resp_len);
		r->io->eapResp = true;
	}
	r->io->eapReq = r->io->eapNoReq = r->io->eapSuccess = r->io->eapFail = false;
	while (eap_server_sm_step(r->sm) && turns++ < 100)
		;
	*state = r->io->eapSuccess ? 1 : r->io->eapFail ? 2 : 0;
	if ((r->io->eapReq || r->io->eapSuccess || r->io->eapFail) && r->io->eapReqData) {
		n = wpabuf_len(r->io->eapReqData);
		if (n > max)
			return 0;
		os_memcpy(out, wpabuf_head(r->io->eapReqData), n);
	}
	return n;
}

/* The MSK the server derived (64 bytes), or 0 if it has none. */
size_t ref_key(struct ref *r, u8 *out)
{
	if (!r->io->eapKeyAvailable || !r->io->eapKeyData || r->io->eapKeyDataLen < 64)
		return 0;
	os_memcpy(out, r->io->eapKeyData, 64);
	return 64;
}

void ref_free(struct ref *r)
{
	if (!r)
		return;
	if (r->sm)
		eap_server_sm_deinit(r->sm);
	if (r->tls)
		tls_deinit(r->tls);
	os_free(r);
}
