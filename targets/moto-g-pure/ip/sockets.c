// TCP sockets for open apps, served from Radio's event loop through a region
// shared with the Open Runtime (runtime/open/net.rs has the same layout).
//
// Page 0: word 0 magic "NET1", word 1 how Radio is online (0 not, 1 on a
// joined network, 2 through the phone's relay). Slots of 32 words follow from
// byte 256: 0 command serial and 1 command (Open Runtime), 2 serial done and
// 3 state and 4 error (Radio), 5 IPv4 address and 6 port, 7 bytes queued to
// send and 9 bytes received in total (written by their producers), 8 bytes
// sent and 10 bytes read (written by their consumers), 11 shut down sending;
// bytes 64-127 a host name to resolve instead of the address. Each slot owns
// an 8 KiB send ring and a 16 KiB receive ring from page 1 on.
#include "lwip/dns.h"
#include "lwip/tcp.h"
#include "lwip/timeouts.h"
#include <string.h>

enum { SLOTS = 8, TX = 8192, RX = 16384, STRIDE = TX + RX };
enum { CMD_CONNECT = 1, CMD_CLOSE = 2 };
enum { IDLE, CONNECTING, OPEN, PEER_CLOSED, FAILED };
enum { E_RESET = 104, E_REFUSED = 111, E_TIMEOUT = 110, E_UNREACH = 113, E_NONAME = 2 };

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
  if (s[i].pcb) {
    tcp_arg(s[i].pcb, NULL);
    tcp_recv(s[i].pcb, NULL);
    tcp_err(s[i].pcb, NULL);
    if (tcp_close(s[i].pcb) != ERR_OK) tcp_abort(s[i].pcb);
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
