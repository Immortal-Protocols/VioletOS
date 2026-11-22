// TLS, certificate validation and UUID parsing run in Radio on the guest.
#include "isrg_x1.h"
#include <bearssl.h>
#include <string.h>
extern int radio_entropy(unsigned char seed[32]);
extern unsigned radio_unix_time(void);
extern int tunnel_read(void *, unsigned char *, size_t);
extern int tunnel_write(void *, const unsigned char *, size_t);
static br_ssl_client_context client;
static br_x509_minimal_context verifier;
static unsigned char buffer[BR_SSL_BUFSIZE_MONO];
static unsigned char headers[16384];
static int hex(unsigned char c) {
  return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') ||
         (c >= 'A' && c <= 'F');
}
static int equal_ascii(const unsigned char *a, size_t n, const char *b) {
  if (n != strlen(b))
    return 0;
  for (size_t i = 0; i < n; i++)
    if ((a[i] >= 'A' && a[i] <= 'Z' ? a[i] + 32 : a[i]) != b[i])
      return 0;
  return 1;
}
static int finish_body(const unsigned char *body, size_t size,
                       unsigned char uuid[36]) {
  if (size && body[size - 1] == '\n')
    size--;
  if (size && body[size - 1] == '\r')
    size--;
  if (size != 36)
    return 0;
  for (unsigned i = 0; i < 36; i++) {
    if (i == 8 || i == 13 || i == 18 || i == 23) {
      if (body[i] != '-')
        return 0;
    } else if (!hex(body[i]))
      return 0;
  }
  if (body[14] != '4' || (body[19] != '8' && body[19] != '9' &&
                          (body[19] | 32) != 'a' && (body[19] | 32) != 'b'))
    return 0;
  memcpy(uuid, body, 36);
  return 1;
}
// Read an HTTP/1.1 body of at most `cap` bytes, framed by Content-Length,
// chunked encoding or connection close.
static int read_body(br_sslio_context *io, size_t n, unsigned char *body,
                     size_t cap, size_t *size) {
  long length = -1;
  int chunked = 0;
  for (size_t pos = 0; pos + 1 < n;) {
    size_t end = pos;
    while (end + 1 < n && !(headers[end] == '\r' && headers[end + 1] == '\n'))
      end++;
    size_t colon = pos;
    while (colon < end && headers[colon] != ':')
      colon++;
    size_t val = colon + 1;
    while (val < end && (headers[val] == ' ' || headers[val] == '\t'))
      val++;
    if (equal_ascii(headers + pos, colon - pos, "content-length")) {
      if (length >= 0 || val == end)
        return 0;
      length = 0;
      for (; val < end; val++) {
        if (headers[val] < '0' || headers[val] > '9')
          return 0;
        length = length * 10 + headers[val] - '0';
        if (length > (long)cap)
          return 0;
      }
    } else if (equal_ascii(headers + pos, colon - pos, "transfer-encoding")) {
      if (chunked || !equal_ascii(headers + val, end - val, "chunked"))
        return 0;
      chunked = 1;
    }
    pos = end + 2;
  }
  if (chunked) {
    if (length >= 0)
      return 0;
    size_t used = 0;
    for (;;) {
      unsigned char c = 0;
      unsigned count = 0, digits = 0;
      while (br_sslio_read_all(io, &c, 1) == 0 && hex(c)) {
        count = count * 16 + (c <= '9' ? c - '0' : (c | 32) - 'a' + 10);
        if (++digits > 4 || count > cap - used)
          return 0;
      }
      if (!digits || c != '\r' || br_sslio_read_all(io, &c, 1) < 0 || c != '\n')
        return 0;
      if (!count) {
        *size = used;
        return 1;
      }
      if (br_sslio_read_all(io, body + used, count) < 0)
        return 0;
      unsigned char crlf[2];
      if (br_sslio_read_all(io, crlf, 2) < 0 || memcmp(crlf, "\r\n", 2))
        return 0;
      used += count;
    }
  }
  if (length >= 0) {
    *size = length;
    return br_sslio_read_all(io, body, length) == 0;
  }
  size_t used = 0;
  unsigned char c;
  while (br_sslio_read(io, &c, 1) > 0) {
    if (used == cap)
      return 0;
    body[used++] = c;
  }
  *size = used;
  return br_ssl_engine_last_error(&client.eng) == 0;
}
// HTTPS GET over the radio tunnel. The certificate must chain to ISRG Root X1
// and match `host`; only a 200 response is accepted.
static int https_get(const char *host, const char *path, unsigned char *body,
                     size_t cap, size_t *size) {
  static const uint16_t suites[] = {BR_TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
                                    BR_TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256};
  unsigned char seed[32];
  if (!radio_entropy(seed))
    return 0;
  br_ssl_client_init_full(&client, &verifier, TAs, TAs_NUM);
  static const char *protocols[] = {"http/1.1"};
  br_ssl_engine_set_protocol_names(&client.eng, protocols, 1);
  br_ssl_engine_set_versions(&client.eng, BR_TLS12, BR_TLS12);
  br_ssl_engine_set_suites(&client.eng, suites, 2);
  unsigned now = radio_unix_time();
  if (!now) return 0;
  br_x509_minimal_set_time(&verifier, 719528 + now / 86400, now % 86400);
  br_ssl_engine_inject_entropy(&client.eng, seed, sizeof(seed));
  memset(seed, 0, sizeof(seed));
  br_ssl_engine_set_buffer(&client.eng, buffer, sizeof(buffer), 0);
  if (!br_ssl_client_reset(&client, host, 0))
    return 0;
  br_sslio_context io;
  br_sslio_init(&io, &client.eng, tunnel_read, 0, tunnel_write, 0);
  static const char *const parts[] = {
      "GET ", 0, " HTTP/1.1\r\nHost: ", 0,
      "\r\nUser-Agent: VioletOS/1.0\r\nAccept: text/plain\r\nCache-Control: "
      "no-cache\r\nConnection: close\r\n\r\n"};
  for (unsigned i = 0; i < 5; i++) {
    const char *part = i == 1 ? path : i == 3 ? host : parts[i];
    if (br_sslio_write_all(&io, part, strlen(part)) < 0)
      return 0;
  }
  if (br_sslio_flush(&io) < 0)
    return 0;
  size_t n = 0;
  do {
    if (n == sizeof(headers) || br_sslio_read_all(&io, headers + n, 1) < 0)
      return 0;
    n++;
  } while (n < 4 || memcmp(headers + n - 4, "\r\n\r\n", 4));
  if (n < 16 || (memcmp(headers, "HTTP/1.0 200 ", 13) &&
                 memcmp(headers, "HTTP/1.1 200 ", 13)))
    return 0;
  return read_body(&io, n, body, cap, size);
}
int guest_https(unsigned char uuid[36]) {
  unsigned char body[38];
  size_t size;
  return https_get("www.uuidgenerator.net", "/api/version4", body, sizeof(body),
                   &size) &&
         finish_body(body, size, uuid);
}
// Fetch a short plain-text reply and keep only what the bitmap font can draw:
// upper-case letters, digits, ": - . /", spaces and the ';' field separator.
int guest_https_text(const char *host, const char *path, unsigned char *out,
                     size_t cap) {
  unsigned char body[256];
  size_t size = 0, n = 0;
  if (!https_get(host, path, body, sizeof(body), &size))
    return 0;
  for (size_t i = 0; i < size && n < cap; i++) {
    unsigned char c = body[i];
    if (c >= 'a' && c <= 'z')
      c -= 32;
    if ((c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == ':' ||
        c == '-' || c == '.' || c == '/' || c == ';') {
      out[n++] = c;
    } else if (c == ' ' && n && out[n - 1] != ' ' && out[n - 1] != ';') {
      out[n++] = ' ';
    }
  }
  while (n && out[n - 1] == ' ')
    n--;
  return (int)n;
}
