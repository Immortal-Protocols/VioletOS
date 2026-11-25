// TCP and UDP sockets for open apps, served from Radio's event loop through a
// region shared with the Open Runtime (runtime/open/net.rs has the same layout).
//
// Page 0: word 0 magic "NET1", word 1 how Radio is online (0 not, 1 on a
// joined network, 2 through the phone's relay). Slots of 32 words follow from
// byte 256: 0 command serial and 1 command (Open Runtime), 2 serial done and
// 3 state and 4 error (Radio), 5 IPv4 address and 6 port, 7 bytes queued to
// send and 9 bytes received in total (written by their producers), 8 bytes
// sent and 10 bytes read (written by their consumers), 11 shut down sending;
// bytes 64-127 a host name to resolve instead of the address. Each slot owns
// an 8 KiB send ring and a 16 KiB receive ring from page 1 on.
//
// A UDP slot (command 3) carries datagrams in its rings: a little-endian u16
// length of the rest, the IPv4 address (u32) and port (u16), a name length
// (u8) and the name (sending only: a name is resolved instead of the
// address), then the payload. Word 12 is the socket's local port and 13 the
// address this phone has on the network. Through the phone's relay (SOCKS,
// TCP only) a UDP slot fails with ENETUNREACH.
#include "lwip/dns.h"
#include "lwip/tcp.h"
#include "lwip/udp.h"
#include "lwip/netif.h"
#include "lwip/timeouts.h"
#include <string.h>

enum { SLOTS = 8, TX = 8192, RX = 16384, STRIDE = TX + RX };
enum { CMD_CONNECT = 1, CMD_CLOSE = 2, CMD_UDP = 3 };
enum { IDLE, CONNECTING, OPEN, PEER_CLOSED, FAILED };
enum { E_RESET = 104, E_REFUSED = 111, E_TIMEOUT = 110, E_UNREACH = 113, E_NONAME = 2, E_NETUNREACH = 101 };
enum { MAX_DATAGRAM = 1472 };

struct slot {
  unsigned cmd_seq, cmd, done_seq, state, error, ip, port, tx_head, tx_tail, rx_head, rx_tail, shut;
  unsigned reserved[4];
  char name[64];
};
static volatile unsigned *page;
static struct {
  struct tcp_pcb *pcb;
  struct pbuf *rx;
  int connected, failed, eof, resolving;
  ip_addr_t addr;
  struct udp_pcb *udp;
  // The last name a UDP slot sent to, and its address.
  char cached[64];
  ip_addr_t cached_addr;
} s[SLOTS];
extern void station_net_poll(void);
extern unsigned radio_monotonic_ms(void);
extern int station_relay_info(unsigned char *password, unsigned *len);

static volatile struct slot *slot(int i) { return (volatile struct slot *)(page + 64) + i; }
static unsigned char *tx_ring(int i) { return (unsigned char *)page + 4096 + i * STRIDE; }
static unsigned char *rx_ring(int i) { return tx_ring(i) + TX; }

static err_t on_connected(void *arg, struct tcp_pcb *pcb, err_t err) {
  int i = (int)(uintptr_t)arg;
  s[i].connected = err == ERR_OK;
  s[i].failed = err != ERR_OK;
  return ERR_OK;
}
static void on_error(void *arg, err_t err) {
  int i = (int)(uintptr_t)arg;
  s[i].pcb = NULL;
  s[i].failed = 1;
}
static err_t on_received(void *arg, struct tcp_pcb *pcb, struct pbuf *p, err_t err) {
  int i = (int)(uintptr_t)arg;
  if (!p) {
    s[i].eof = 1;
    return ERR_OK;
  }
  if (s[i].rx) pbuf_cat(s[i].rx, p);
  else s[i].rx = p;
  return ERR_OK;
}
static void on_resolved(const char *name, const ip_addr_t *addr, void *arg) {
  int i = (int)(uintptr_t)arg;
  if (!s[i].resolving) return;
  s[i].resolving = 0;
  if (addr) s[i].addr = *addr;
  else s[i].failed = 1;
}

static void drop(int i) {
  if (s[i].udp) udp_remove(s[i].udp);
  if (s[i].pcb) {
    tcp_arg(s[i].pcb, NULL);
    tcp_recv(s[i].pcb, NULL);
    tcp_err(s[i].pcb, NULL);
    // A connection closed with data still unsent or unacknowledged (its peer
    // went quiet) would keep that data in lwIP's 24 KiB heap until its
    // retransmissions give up, minutes later; a few of them starve every new
    // connection's tcp_write. Reset those at once; close the rest gracefully.
    if (s[i].pcb->unsent || s[i].pcb->unacked || tcp_close(s[i].pcb) != ERR_OK) tcp_abort(s[i].pcb);
  }
  if (s[i].rx) pbuf_free(s[i].rx);
  memset(&s[i], 0, sizeof s[i]);
}
static int wait_until(int i, int *flag, unsigned ms) {
  unsigned start = radio_monotonic_ms();
  while (!*flag && !s[i].failed && radio_monotonic_ms() - start < ms) station_net_poll();
  return *flag && !s[i].failed;
}
static int tcp_open(int i, const ip_addr_t *addr, unsigned port) {
  s[i].pcb = tcp_new();
  if (!s[i].pcb) return 0;
  tcp_arg(s[i].pcb, (void *)(uintptr_t)i);
  tcp_err(s[i].pcb, on_error);
  tcp_recv(s[i].pcb, on_received);
  if (tcp_connect(s[i].pcb, addr, port, on_connected) != ERR_OK) return 0;
  return wait_until(i, &s[i].connected, 8000);
}
// Blocking reads and writes on a slot's connection, for the relay handshake.
static int put(int i, const unsigned char *p, unsigned n) {
  unsigned start = radio_monotonic_ms();
  while (n && s[i].pcb && !s[i].failed && radio_monotonic_ms() - start < 5000) {
    unsigned k = tcp_sndbuf(s[i].pcb);
    if (k > n) k = n;
    if (k && tcp_write(s[i].pcb, p, k, TCP_WRITE_FLAG_COPY) == ERR_OK) {
      tcp_output(s[i].pcb);
      p += k;
      n -= k;
    } else {
      station_net_poll();
    }
  }
  return n == 0;
}
static int get(int i, unsigned char *p, unsigned n) {
  unsigned start = radio_monotonic_ms();
  while (n && !s[i].failed && radio_monotonic_ms() - start < 5000) {
    if (!s[i].rx) {
      if (s[i].eof) return 0;
      station_net_poll();
      continue;
    }
    unsigned k = s[i].rx->tot_len < n ? s[i].rx->tot_len : n;
    k = pbuf_copy_partial(s[i].rx, p, k, 0);
    s[i].rx = pbuf_free_header(s[i].rx, k);
    tcp_recved(s[i].pcb, k);
    p += k;
    n -= k;
  }
  return n == 0;
}
// SOCKS5 CONNECT through Violet Relay on the phone (RFC 1928, RFC 1929),
// by name when there is one so the phone resolves it.
static int relay_connect(int i, const char *name, unsigned ip, unsigned port) {
  ip_addr_t phone;
  IP4_ADDR(ip_2_ip4(&phone), 192, 168, 77, 2);
  unsigned char b[300], password[64];
  unsigned plen = 0;
  if (!station_relay_info(password, &plen) || !tcp_open(i, &phone, 1081)) return 0;
  static const unsigned char hello[] = {5, 1, 2};
  if (!put(i, hello, 3) || !get(i, b, 2) || b[0] != 5 || b[1] != 2) return 0;
  unsigned n = 0;
  b[n++] = 1;
  b[n++] = 6;
  memcpy(b + n, "violet", 6);
  n += 6;
  b[n++] = plen;
  memcpy(b + n, password, plen);
  n += plen;
  if (!put(i, b, n) || !get(i, b, 2) || b[1] != 0) return 0;
  n = 0;
  b[n++] = 5; b[n++] = 1; b[n++] = 0;
  if (name[0]) {
    unsigned hl = strlen(name);
    b[n++] = 3;
    b[n++] = hl;
    memcpy(b + n, name, hl);
    n += hl;
  } else {
    b[n++] = 1;
    b[n++] = ip >> 24; b[n++] = ip >> 16; b[n++] = ip >> 8; b[n++] = ip;
  }
  b[n++] = port >> 8;
  b[n++] = port;
  if (!put(i, b, n) || !get(i, b, 4) || b[0] != 5) return 0;
  if (b[1] != 0) {
    s[i].failed = 1;
    return 0;
  }
  unsigned rest = b[3] == 1 ? 6 : b[3] == 4 ? 18 : 0;
  if (b[3] == 3) {
    if (!get(i, b, 1)) return 0;
    rest = b[0] + 2;
  }
  return rest && get(i, b, rest);
}
static int direct_connect(int i, const char *name, unsigned ip, unsigned port) {
  if (name[0]) {
    s[i].resolving = 1;
    err_t rc = dns_gethostbyname(name, &s[i].addr, on_resolved, (void *)(uintptr_t)i);
    if (rc == ERR_OK) s[i].resolving = 0;
    else if (rc != ERR_INPROGRESS) return 0;
    unsigned start = radio_monotonic_ms();
    while (s[i].resolving && !s[i].failed && radio_monotonic_ms() - start < 5000) station_net_poll();
    if (s[i].resolving || s[i].failed) {
      s[i].resolving = 0;
      return 0;
    }
  } else {
    IP4_ADDR(ip_2_ip4(&s[i].addr), ip >> 24, ip >> 16 & 255, ip >> 8 & 255, ip & 255);
  }
  return tcp_open(i, &s[i].addr, port);
}

static unsigned host_order(const ip_addr_t *a) { return __builtin_bswap32(ip4_addr_get_u32(ip_2_ip4(a))); }

// A datagram for a UDP slot: straight into its receive ring, or dropped when
// the ring is full (the app is not reading).
static void on_datagram(void *arg, struct udp_pcb *pcb, struct pbuf *p, const ip_addr_t *addr, u16_t port) {
  int i = (int)(uintptr_t)arg;
  volatile struct slot *q = slot(i);
  unsigned n = p->tot_len, in = q->rx_head, out = q->rx_tail;
  if (n <= MAX_DATAGRAM && RX - (in - out) >= n + 9) {
    unsigned char h[9];
    unsigned ip = host_order(addr), body = n + 7;
    h[0] = body; h[1] = body >> 8;
    h[2] = ip; h[3] = ip >> 8; h[4] = ip >> 16; h[5] = ip >> 24;
    h[6] = port; h[7] = port >> 8; h[8] = 0;
    unsigned char *r = rx_ring(i);
    for (unsigned k = 0; k < 9; k++) r[(in + k) % RX] = h[k];
    in += 9;
    for (struct pbuf *b = p; b; b = b->next)
      for (unsigned k = 0; k < b->len; k++) r[(in++) % RX] = ((unsigned char *)b->payload)[k];
    __asm__ volatile("dmb ish" ::: "memory");
    q->rx_head = in;
  }
  pbuf_free(p);
}

static int udp_start(int i) {
  s[i].udp = udp_new();
  if (!s[i].udp) return 0;
  if (udp_bind(s[i].udp, IP4_ADDR_ANY, 0) != ERR_OK) return 0;
  udp_recv(s[i].udp, on_datagram, (void *)(uintptr_t)i);
  slot(i)->reserved[0] = s[i].udp->local_port;
  return 1;
}

// A name a UDP slot sends to, resolved (and remembered) by Radio.
static int udp_resolve(int i, const char *name, ip_addr_t *to) {
  unsigned n = strlen(name);
  if (n < sizeof s[i].cached && s[i].cached[0] && !memcmp(s[i].cached, name, n + 1)) {
    *to = s[i].cached_addr;
    return 1;
  }
  s[i].resolving = 1;
  s[i].failed = 0;
  err_t rc = dns_gethostbyname(name, &s[i].addr, on_resolved, (void *)(uintptr_t)i);
  if (rc == ERR_OK) s[i].resolving = 0;
  else if (rc != ERR_INPROGRESS) return 0;
  unsigned start = radio_monotonic_ms();
  while (s[i].resolving && !s[i].failed && radio_monotonic_ms() - start < 5000) station_net_poll();
  if (s[i].resolving || s[i].failed || !s[i].udp) {
    s[i].resolving = 0;
    s[i].failed = 0;
    return 0;
  }
  if (n < sizeof s[i].cached) memcpy(s[i].cached, name, n + 1);
  s[i].cached_addr = s[i].addr;
  *to = s[i].addr;
  return 1;
}

static void pump_udp(int i) {
  volatile struct slot *q = slot(i);
  if (netif_default) q->reserved[1] = host_order(&netif_default->ip_addr);
  unsigned head = q->tx_head, tail = q->tx_tail;
  static unsigned char f[7 + 64 + MAX_DATAGRAM];
  const unsigned char *t = tx_ring(i);
  while (head - tail >= 2 && s[i].udp) {
    unsigned body = t[tail % TX] | t[(tail + 1) % TX] << 8;
    if (body < 7 || body > sizeof f) {
      tail = head;  // not a datagram: drop what is queued
      break;
    }
    if (head - tail < body + 2) break;
    for (unsigned k = 0; k < body; k++) f[k] = t[(tail + 2 + k) % TX];
    tail += body + 2;
    unsigned ip = f[0] | f[1] << 8 | f[2] << 16 | (unsigned)f[3] << 24, port = f[4] | f[5] << 8, nl = f[6];
    if (7 + nl > body || nl > 63) continue;
    ip_addr_t to;
    if (nl) {
      char name[64];
      memcpy(name, f + 7, nl);
      name[nl] = 0;
      if (!udp_resolve(i, name, &to)) continue;
    } else {
      IP4_ADDR(ip_2_ip4(&to), ip >> 24, ip >> 16 & 255, ip >> 8 & 255, ip & 255);
    }
    unsigned n = body - 7 - nl;
    struct pbuf *p = pbuf_alloc(PBUF_TRANSPORT, n, PBUF_RAM);
    if (!p) continue;
    memcpy(p->payload, f + 7 + nl, n);
    udp_sendto(s[i].udp, p, &to, port);
    pbuf_free(p);
  }
  q->tx_tail = tail;
}

static void command(int i, unsigned online) {
  volatile struct slot *q = slot(i);
  unsigned cmd = q->cmd;
  drop(i);
  if (cmd == CMD_CLOSE) {
    q->state = IDLE;
    return;
  }
  char name[64];
  for (int k = 0; k < 63; k++) name[k] = q->name[k];
  name[63] = 0;
  q->state = CONNECTING;
  q->error = 0;
  if (cmd == CMD_UDP) {
    if (online != 2 && udp_start(i)) {
      q->state = OPEN;
      return;
    }
    drop(i);
    q->error = E_NETUNREACH;
    q->state = FAILED;
    return;
  }
  int ok = online == 2 ? relay_connect(i, name, q->ip, q->port) : direct_connect(i, name, q->ip, q->port);
  if (ok) {
    q->state = OPEN;
    return;
  }
  q->error = name[0] && !s[i].connected && !s[i].pcb ? E_NONAME : s[i].failed ? E_REFUSED : E_TIMEOUT;
  if (online == 2 && s[i].failed) q->error = E_REFUSED;
  drop(i);
  q->state = FAILED;
}

static void pump(int i) {
  volatile struct slot *q = slot(i);
  if (q->state != OPEN && q->state != PEER_CLOSED) return;
  if (s[i].udp) return pump_udp(i);
  if (!s[i].pcb) {
    q->state = FAILED;
    q->error = E_RESET;
    return;
  }
  // Bytes from the app to the network.
  unsigned head = q->tx_head, tail = q->tx_tail;
  while (head != tail) {
    unsigned n = head - tail, at = tail % TX, room = tcp_sndbuf(s[i].pcb);
    if (n > TX - at) n = TX - at;
    if (n > room) n = room;
    if (!n || tcp_write(s[i].pcb, tx_ring(i) + at, n, TCP_WRITE_FLAG_COPY) != ERR_OK) break;
    tail += n;
  }
  if (tail != q->tx_tail) {
    q->tx_tail = tail;
    tcp_output(s[i].pcb);
  }
  if (q->shut == 1 && tail == head) {
    tcp_shutdown(s[i].pcb, 0, 1);
    q->shut = 2;
  }
  // Bytes from the network to the app, as the ring has room.
  unsigned in = q->rx_head, out = q->rx_tail;
  while (s[i].rx && in - out < RX) {
    unsigned at = in % RX, n = RX - (in - out);
    if (n > RX - at) n = RX - at;
    if (n > s[i].rx->tot_len) n = s[i].rx->tot_len;
    n = pbuf_copy_partial(s[i].rx, rx_ring(i) + at, n, 0);
    s[i].rx = pbuf_free_header(s[i].rx, n);
    tcp_recved(s[i].pcb, n);
    in += n;
  }
  if (in != q->rx_head) {
    __asm__ volatile("dmb ish" ::: "memory");
    q->rx_head = in;
  }
  if (s[i].eof && !s[i].rx) q->state = PEER_CLOSED;
}

// Called from Radio's event loop. `online` is 0, 1 (joined network) or 2 (relay).
void open_sockets_poll(void *base, unsigned online) {
  page = base;
  if (page[0] != 0x3154454e) {
    memset((void *)page, 0, 4096);
    page[0] = 0x3154454e;  // "NET1"
  }
  if (page[1] != online) {
    page[1] = online;
    if (!online)
      for (int i = 0; i < SLOTS; i++) {
        drop(i);
        if (slot(i)->state != IDLE) {
          slot(i)->state = FAILED;
          slot(i)->error = E_UNREACH;
        }
      }
  }
  for (int i = 0; i < SLOTS; i++) {
    volatile struct slot *q = slot(i);
    unsigned seq = q->cmd_seq;
    if (seq != q->done_seq) {
      if (online) command(i, online);
      else {
        q->state = FAILED;
        q->error = E_UNREACH;
      }
      q->done_seq = seq;
    }
    if (online) pump(i);
  }
  if (online) sys_check_timeouts();
}
