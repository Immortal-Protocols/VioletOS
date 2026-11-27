// TCP and UDP sockets for open apps, served from Radio's event loop through a
// region shared with the Open Runtime (runtime/open/net.rs has the same
// layout, "NET2"; tools/test_net_layout.py checks the two and the emulator's
// relay agree).
//
// Page 0: word 0 the magic, word 1 how Radio is online (0 not, 1 on a joined
// network, 2 through the phone's relay), word 2 the slots served, words 3 and
// 4 the ring sizes, word 5 how many bytes of the region the supervisor mapped
// (the supervisor writes it; Radio serves only the slots that fit, and the 7
// that fit the 49 pages of a supervisor that says nothing). Slot headers of
// 512 bytes follow from page 1, 16 words each: 0 command serial and
// 1 command (Open Runtime), 2 serial done and 3 state and 4 error (Radio),
// 5 IPv4 address and 6 port, 7 bytes queued to send and 9 bytes received in
// total (written by their producers), 8 bytes sent and 10 bytes read (written
// by their consumers), 11 shut down sending, 14 who the socket is for (0 the
// Open Runtime itself, else an app; Radio only counts by it); from byte 64 a
// host name to resolve instead of the address (up to 253 bytes and a NUL);
// the rest is spare. Radio alone writes words 8, 9, 12 and 13, and empties
// them when it takes a command: until then (serial done != command serial)
// they are the previous use's, and the Open Runtime reads nothing from the
// receive ring (what it queues to send before then is the new use's, counted
// from zero). Each slot owns an 8 KiB send ring and
// a 16 KiB receive ring: slots 0-8 from byte 0x5000, slots 9-31 from byte
// 0x43000 (the Bluetooth pages sit between the two runs).
//
// A UDP slot (command 3) carries datagrams in its rings: a little-endian u16
// length of the rest, the IPv4 address (u32) and port (u16), a name length
// (u8) and the name (sending only: a name is resolved instead of the
// address), then the payload. Word 12 is the socket's local port and 13 the
// address this phone has on the network. Through the phone's relay (SOCKS,
// TCP only) a UDP slot fails with ENETUNREACH.
//
// Nothing here waits. A connect is a state machine stepped once per call of
// open_sockets_poll (resolve, connect, then the relay's handshake), each step
// with a deadline, so a name that does not resolve or a host that does not
// answer delays no other slot, nor the rest of Radio's loop. Received bytes
// are copied into the slot's ring as they arrive and the TCP window follows
// what the app has read, so a slot holds none of lwIP's buffers; what a slot
// may queue for sending is its share of lwIP's heap. A connection that is
// closing gives its lwIP connection up to the next connect that needs one, and
// a name lwIP is still asking for counts against the app that asked.
#include "lwip/dns.h"
#include "lwip/tcp.h"
#include "lwip/udp.h"
#include "lwip/netif.h"
#include "lwip/timeouts.h"
#include <string.h>

enum { SLOTS = 32, TX = 8192, RX = 16384, STRIDE = TX + RX };
enum { HEADERS = 0x1000, HEADER = 512, NAME = 256, RINGS_A = 0x5000, FIRST = 9, RINGS_B = 0x43000 };
enum { MAGIC = 0x3254454e };  // "NET2"
enum { CMD_CONNECT = 1, CMD_CLOSE = 2, CMD_UDP = 3 };
enum { IDLE, CONNECTING, OPEN, PEER_CLOSED, FAILED };
enum {
  E_NONAME = 2, E_INVALID = 22, E_NETUNREACH = 101, E_RESET = 104, E_NOBUFS = 105, E_TIMEOUT = 110,
  E_REFUSED = 111, E_UNREACH = 113
};
enum { MAX_DATAGRAM = 1472, MAX_NAME = 253 };
// How long a step of a connect may take.
enum { RESOLVE_MS = 5000, CONNECT_MS = 8000, RELAY_MS = 5000 };
// The least a sending slot may have queued in lwIP, whatever its share.
enum { MIN_SHARE = 256 };
// The region of a supervisor that does not say how much it mapped (49 pages).
enum { LEGACY_EXTENT = 0x31000, MAX_EXTENT = 0x100000 };
// Names lwIP is asking for on behalf of slots. lwIP cannot be told to forget
// a question, so one outlives the slot that asked it (for up to 14 s); what is
// counted here is therefore the questions, not the slots. One owner has at
// most ASKS_EACH out, all apps together APP_ASKS, which leaves lwIP's table
// (DNS_TABLE_SIZE) ASKS_EACH for the Open Runtime's own sockets and the rest
// for Radio itself.
enum { ASKS_EACH = 4, APP_ASKS = 32, ASKS = APP_ASKS + ASKS_EACH };
_Static_assert(DNS_TABLE_SIZE >= ASKS + 4, "lwIP's table of names is smaller than the questions slots may ask");

struct slot {
  unsigned cmd_seq, cmd, done_seq, state, error, ip, port, tx_head, tx_tail, rx_head, rx_tail, shut;
  unsigned local_port, local_ip, owner, reserved;
  char name[NAME];
  unsigned char spare[HEADER - 64 - NAME];
};
_Static_assert(sizeof(struct slot) == HEADER, "slot header size");
// A whole window of received bytes always fits an empty receive ring.
_Static_assert(RX >= TCP_WND, "receive ring smaller than the TCP window");

// Where a slot's connect has got to.
enum { P_IDLE, P_RESOLVE, P_CONNECT, P_HELLO, P_AUTH, P_REPLY, P_UP };
// A name lookup: not asked, asked, answered, failed.
enum { D_NONE, D_WAIT, D_OK, D_FAILED };

static volatile unsigned *page;
static struct state {
  struct tcp_pcb *pcb;
  struct udp_pcb *udp;
  ip_addr_t addr;
  // When the current step gives up (radio_monotonic_ms).
  unsigned deadline;
  // Relay handshake bytes held in the receive ring, not yet the app's.
  unsigned staged;
  // Received bytes the TCP window has not been reopened for.
  unsigned owed;
  // Counts this slot's uses: a callback from an earlier use is ignored.
  unsigned gen;
  // Who the socket is for (the header's word 14, read when the command is taken).
  unsigned owner;
  // The question this slot waits on, plus one (asks[]).
  unsigned char ask;
  unsigned char phase, dns, sent, connected, failed, eof, relay, waiting, shut;
} s[SLOTS];
static struct ask {
  unsigned owner, gen;
  // The slot waiting for the answer, or -1 once it has moved on.
  int slot;
  unsigned char used;
} asks[ASKS];
// Slots served: those whose rings the supervisor mapped. Set on the first poll.
static unsigned count;
// What one slot may have queued in lwIP for sending, this poll.
static unsigned share;
extern unsigned radio_monotonic_ms(void);
extern int station_relay_info(unsigned char *password, unsigned *len);

static volatile struct slot *slot(int i) { return (volatile struct slot *)((unsigned char *)page + HEADERS) + i; }
static unsigned ring_offset(int i) { return i < FIRST ? RINGS_A + i * STRIDE : RINGS_B + (i - FIRST) * STRIDE; }
static unsigned char *tx_ring(int i) { return (unsigned char *)page + ring_offset(i); }
// The slots that lie wholly inside `extent` bytes of the region.
static unsigned fitting(unsigned extent) {
  if (extent % 0x1000 || extent < LEGACY_EXTENT || extent > MAX_EXTENT) extent = LEGACY_EXTENT;
  unsigned n = 0;
  while (n < SLOTS && ring_offset(n) + STRIDE <= extent) n++;
  return n;
}
static unsigned char *rx_ring(int i) { return tx_ring(i) + TX; }
static void barrier(void) {
#ifdef __arm__
  __asm__ volatile("dmb ish" ::: "memory");
#else
  __sync_synchronize();
#endif
}
static int expired(int i, unsigned now) { return (int)(now - s[i].deadline) >= 0; }

// The argument lwIP hands back to a callback: the slot and its use count.
static void *tag(int i) { return (void *)(uintptr_t)((unsigned)i | s[i].gen << 8); }
static int tagged(void *arg) {
  unsigned v = (unsigned)(uintptr_t)arg, i = v & 255;
  return i < SLOTS && s[i].gen == v >> 8 ? (int)i : -1;
}

static void drop(int i) {
  if (s[i].udp) udp_remove(s[i].udp);
  // The question it waits on stays lwIP's, and its owner's, until answered.
  if (s[i].ask) asks[s[i].ask - 1].slot = -1;
  if (s[i].pcb) {
    tcp_arg(s[i].pcb, NULL);
    tcp_recv(s[i].pcb, NULL);
    tcp_err(s[i].pcb, NULL);
    // A closed connection stays in lwIP until its peer has closed too (or for
    // 20 s), and a peer may never: an app that opens and closes in a loop
    // would take every connection lwIP has (MEMP_NUM_TCP_PCB). At the lowest
    // priority lwIP gives a closing connection up, oldest first, to the next
    // tcp_new that finds none free (tcp_kill_prio), so only connections that
    // slots hold count. Set before tcp_close, which may free the pcb.
    tcp_setprio(s[i].pcb, TCP_PRIO_MIN);
    // A connection closed with data still unsent or unacknowledged (its peer
    // went quiet) would keep that data in lwIP's heap until its
    // retransmissions give up, minutes later. Reset those at once; close the
    // rest gracefully.
    if (s[i].pcb->unsent || s[i].pcb->unacked || tcp_close(s[i].pcb) != ERR_OK) tcp_abort(s[i].pcb);
  }
  unsigned gen = (s[i].gen + 1) & 0xffffff;
  memset(&s[i], 0, sizeof s[i]);
  s[i].gen = gen;
}
static void fail(int i, unsigned error) {
  drop(i);
  slot(i)->error = error;
  slot(i)->state = FAILED;
}

// Reopen the TCP window for what the app has read: the window never offers
// more than the ring has room for.
static void credit(int i) {
  volatile struct slot *q = slot(i);
  unsigned used = q->rx_head - q->rx_tail + s[i].staged;
  unsigned room = used < RX ? RX - used : 0, keep = room >= TCP_WND ? 0 : TCP_WND - room;
  if (s[i].pcb && s[i].owed > keep) {
    unsigned n = s[i].owed - keep;
    if (n > 0xffff) n = 0xffff;
    tcp_recved(s[i].pcb, n);
    s[i].owed -= n;
  }
}

static err_t on_connected(void *arg, struct tcp_pcb *pcb, err_t err) {
  int i = tagged(arg);
  if (i < 0) return ERR_OK;
  s[i].connected = err == ERR_OK;
  s[i].failed = err != ERR_OK;
  return ERR_OK;
}
static void on_error(void *arg, err_t err) {
  int i = tagged(arg);
  if (i < 0) return;
  s[i].pcb = NULL;
  s[i].failed = 1;
}
// Received bytes go straight into the slot's ring: the app's once the
// connection is up, held back (staged) during the relay's handshake.
static err_t on_received(void *arg, struct tcp_pcb *pcb, struct pbuf *p, err_t err) {
  int i = tagged(arg);
  if (i < 0) {
    if (p) pbuf_free(p);
    return ERR_OK;
  }
  if (!p) {
    s[i].eof = 1;
    return ERR_OK;
  }
  volatile struct slot *q = slot(i);
  unsigned head = q->rx_head, used = head - q->rx_tail + s[i].staged, in = head + s[i].staged;
  // Not while the peer keeps to the window. lwIP offers the bytes again.
  if (used > RX || p->tot_len > RX - used) return ERR_MEM;
  unsigned char *r = rx_ring(i);
  for (struct pbuf *b = p; b; b = b->next)
    for (unsigned k = 0; k < b->len; k++) r[(in++) % RX] = ((unsigned char *)b->payload)[k];
  s[i].owed += p->tot_len;
  if (s[i].phase == P_UP) {
    barrier();
    q->rx_head = in;
  } else {
    s[i].staged += p->tot_len;
  }
  pbuf_free(p);
  return ERR_OK;
}
// lwIP is done with a question (answered, or given up): it stops counting,
// and the slot that asked, if it still waits, has its answer.
static void on_resolved(const char *name, const ip_addr_t *addr, void *arg) {
  unsigned v = (unsigned)(uintptr_t)arg, k = v & 255;
  if (k >= ASKS || !asks[k].used || asks[k].gen != v >> 8) return;
  int i = asks[k].slot;
  asks[k].used = 0;
  asks[k].gen = (asks[k].gen + 1) & 0xffffff;
  if (i < 0 || s[i].ask != k + 1) return;
  s[i].ask = 0;
  if (s[i].dns != D_WAIT) return;
  if (addr) s[i].addr = *addr;
  s[i].dns = addr ? D_OK : D_FAILED;
}
// A free place for one more question of `owner`'s, or -1: the owner, or the
// apps together, have as many out as they may.
static int ask_place(unsigned owner) {
  unsigned mine = 0, apps = 0;
  int place = -1;
  for (int k = 0; k < ASKS; k++) {
    if (!asks[k].used) {
      if (place < 0) place = k;
      continue;
    }
    mine += asks[k].owner == owner;
    apps += asks[k].owner != 0;
  }
  if (mine >= ASKS_EACH || (owner && apps >= APP_ASKS)) return -1;
  return place;
}
// A name that is an address written out, in any form lwIP (and a relay's
// resolver) reads as one: "10.0.0.9", "167772169", "0x0a000009", "10.9". The
// Open Runtime never sends one (names.rs); taken as a name it would be
// dialled as an address nobody checked.
static int numeric(const char *name) {
  ip_addr_t literal;
  return ipaddr_aton(name, &literal) != 0;
}

// Ask lwIP for a name's address: the answer is in s[i].dns, now or later
// (still D_NONE: asked again next poll). False when the name cannot be asked
// for at all.
static int resolve(int i, const char *name) {
  if (numeric(name)) return 0;
  int k = ask_place(s[i].owner);
  if (k < 0) return 1;
  err_t rc = dns_gethostbyname(name, &s[i].addr, on_resolved, (void *)(uintptr_t)((unsigned)k | asks[k].gen << 8));
  if (rc == ERR_OK) s[i].dns = D_OK;
  else if (rc == ERR_INPROGRESS) {
    asks[k].used = 1;
    asks[k].owner = s[i].owner;
    asks[k].slot = i;
    s[i].ask = k + 1;
    s[i].dns = D_WAIT;
  }
  // ERR_MEM: every question lwIP can hold is in use. Asked again next poll.
  else if (rc != ERR_MEM) return 0;
  return 1;
}
static void slot_name(int i, char *name) {
  volatile struct slot *q = slot(i);
  for (int k = 0; k < NAME - 1; k++) name[k] = q->name[k];
  name[NAME - 1] = 0;
}

static void start_tcp(int i, const ip_addr_t *addr, unsigned port, unsigned now) {
  s[i].pcb = tcp_new();
  if (!s[i].pcb) return fail(i, E_NOBUFS);
  tcp_arg(s[i].pcb, tag(i));
  tcp_err(s[i].pcb, on_error);
  tcp_recv(s[i].pcb, on_received);
  if (tcp_connect(s[i].pcb, addr, port, on_connected) != ERR_OK) return fail(i, E_UNREACH);
  s[i].phase = P_CONNECT;
  s[i].deadline = now + CONNECT_MS;
}

// A byte of the relay's reply, and taking n of them off the front.
static unsigned staged_byte(int i, unsigned k) { return rx_ring(i)[(slot(i)->rx_head + k) % RX]; }
static void consume(int i, unsigned n) {
  unsigned char *r = rx_ring(i);
  unsigned head = slot(i)->rx_head;
  s[i].staged -= n;
  for (unsigned k = 0; k < s[i].staged; k++) r[(head + k) % RX] = r[(head + k + n) % RX];
  credit(i);
}
// SOCKS5 reply codes as the errno the app sees.
static unsigned socks_error(unsigned code) {
  static const unsigned char errors[] = {0, 5, 13, E_NETUNREACH, E_UNREACH, E_REFUSED, E_TIMEOUT, 95, 97};
  return code < sizeof errors ? errors[code] : E_REFUSED;
}
// What to send the relay in this step (RFC 1928, RFC 1929); 0 if it cannot be built.
static unsigned relay_message(int i, unsigned char *b) {
  unsigned n = 0;
  if (s[i].phase == P_HELLO) {
    b[n++] = 5; b[n++] = 1; b[n++] = 2;
  } else if (s[i].phase == P_AUTH) {
    unsigned char password[64];
    unsigned plen = 0;
    if (!station_relay_info(password, &plen) || plen > sizeof password) return 0;
    b[n++] = 1;
    b[n++] = 6;
    memcpy(b + n, "violet", 6);
    n += 6;
    b[n++] = plen;
    memcpy(b + n, password, plen);
    n += plen;
  } else {
    // CONNECT, by name when there is one so the phone resolves it.
    volatile struct slot *q = slot(i);
    char name[NAME];
    slot_name(i, name);
    unsigned ip = q->ip, port = q->port, hl = strlen(name);
    b[n++] = 5; b[n++] = 1; b[n++] = 0;
    if (hl) {
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
  }
  return n;
}
// The relay's handshake: send this step's message, then take its reply when
// all of it has come. Bytes after the last reply are the connection's.
static void relay_step(int i, unsigned now) {
  volatile struct slot *q = slot(i);
  for (;;) {
    if (s[i].failed || !s[i].pcb) return fail(i, E_REFUSED);
    if (!s[i].sent) {
      unsigned char b[8 + NAME];
      unsigned n = relay_message(i, b);
      if (!n) return fail(i, E_NETUNREACH);
      if (tcp_sndbuf(s[i].pcb) < n || tcp_write(s[i].pcb, b, n, TCP_WRITE_FLAG_COPY) != ERR_OK) break;
      tcp_output(s[i].pcb);
      s[i].sent = 1;
    }
    unsigned need = 2;
    if (s[i].phase == P_REPLY) {
      need = 4;
      if (s[i].staged >= 4) {
        if (staged_byte(i, 0) != 5) return fail(i, E_REFUSED);
        if (staged_byte(i, 1) != 0) return fail(i, socks_error(staged_byte(i, 1)));
        unsigned kind = staged_byte(i, 3);
        if (kind == 1) need = 10;
        else if (kind == 4) need = 22;
        else if (kind != 3) return fail(i, E_REFUSED);
        else need = s[i].staged >= 5 ? 7 + staged_byte(i, 4) : 5;
      }
    }
    if (s[i].staged < need) break;
    if (s[i].phase == P_HELLO && (staged_byte(i, 0) != 5 || staged_byte(i, 1) != 2)) return fail(i, E_REFUSED);
    if (s[i].phase == P_AUTH && staged_byte(i, 1) != 0) return fail(i, 13);
    consume(i, need);
    s[i].sent = 0;
    s[i].deadline = now + RELAY_MS;
    if (++s[i].phase == P_UP) {
      unsigned rest = s[i].staged;
      s[i].staged = 0;
      barrier();
      q->rx_head = q->rx_head + rest;
      q->state = OPEN;
      return;
    }
  }
  if (s[i].eof) return fail(i, E_REFUSED);
  if (expired(i, now)) fail(i, E_TIMEOUT);
}

// One step of a connect that is under way.
static void advance(int i, unsigned now) {
  volatile struct slot *q = slot(i);
  if (s[i].phase == P_RESOLVE) {
    if (s[i].dns == D_NONE) {
      char name[NAME];
      slot_name(i, name);
      if (!resolve(i, name)) return fail(i, E_NONAME);
    }
    if (s[i].dns == D_OK) {
      ip_addr_t addr = s[i].addr;
      s[i].dns = D_NONE;
      return start_tcp(i, &addr, q->port, now);
    }
    if (s[i].dns == D_FAILED || expired(i, now)) fail(i, E_NONAME);
    return;
  }
  if (s[i].phase == P_CONNECT) {
    if (s[i].failed || !s[i].pcb) return fail(i, E_REFUSED);
    if (!s[i].connected) {
      if (expired(i, now)) fail(i, E_TIMEOUT);
      return;
    }
    if (!s[i].relay) {
      // Bytes that came with the connection (a server that speaks first).
      unsigned early = s[i].staged;
      s[i].staged = 0;
      s[i].phase = P_UP;
      barrier();
      q->rx_head = q->rx_head + early;
      q->state = OPEN;
      return;
    }
    s[i].phase = P_HELLO;
    s[i].sent = 0;
    s[i].deadline = now + RELAY_MS;
  }
  relay_step(i, now);
}

static unsigned host_order(const ip_addr_t *a) { return __builtin_bswap32(ip4_addr_get_u32(ip_2_ip4(a))); }

// A datagram for a UDP slot: straight into its receive ring, or dropped when
// the ring is full (the app is not reading).
static void on_datagram(void *arg, struct udp_pcb *pcb, struct pbuf *p, const ip_addr_t *addr, u16_t port) {
  int i = tagged(arg);
  if (i < 0) {
    pbuf_free(p);
    return;
  }
  volatile struct slot *q = slot(i);
  unsigned n = p->tot_len, in = q->rx_head, used = in - q->rx_tail;
  if (n <= MAX_DATAGRAM && used <= RX && RX - used >= n + 9) {
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
    barrier();
    q->rx_head = in;
  }
  pbuf_free(p);
}

static int udp_start(int i) {
  s[i].udp = udp_new();
  if (!s[i].udp) return 0;
  if (udp_bind(s[i].udp, IP4_ADDR_ANY, 0) != ERR_OK) return 0;
  udp_recv(s[i].udp, on_datagram, tag(i));
  slot(i)->local_port = s[i].udp->local_port;
  return 1;
}

// The Open Runtime has given the slot a new command since this poll looked:
// what its send ring holds may be the next use's, and is not this
// connection's to send. (The serial is bumped before anything of the next use
// is queued, so a head read before this check that belongs to it is caught.)
static int superseded(int i) {
  barrier();
  return slot(i)->cmd_seq != slot(i)->done_seq;
}

// Datagrams from the app to the network. One sent to a name waits at the head
// of the ring while lwIP resolves it (lwIP remembers the answer for the
// next), for at most RESOLVE_MS; then it is dropped, as a lost datagram is.
static void pump_udp(int i, unsigned now) {
  volatile struct slot *q = slot(i);
  if (netif_default) q->local_ip = host_order(&netif_default->ip_addr);
  unsigned head = q->tx_head, tail = q->tx_tail;
  if (superseded(i)) return;
  static unsigned char f[7 + 255 + MAX_DATAGRAM];
  const unsigned char *t = tx_ring(i);
  if (head - tail > TX) tail = head;  // not a ring any more: drop what is queued
  while (head - tail >= 2) {
    unsigned body = t[tail % TX] | t[(tail + 1) % TX] << 8;
    if (body < 7 || body > sizeof f) {
      tail = head;  // not a datagram: drop what is queued
      break;
    }
    if (head - tail < body + 2) break;
    for (unsigned k = 0; k < body; k++) f[k] = t[(tail + 2 + k) % TX];
    unsigned ip = f[0] | f[1] << 8 | f[2] << 16 | (unsigned)f[3] << 24, port = f[4] | f[5] << 8, nl = f[6];
    ip_addr_t to;
    if (7 + nl > body || nl > MAX_NAME) {
      tail += body + 2;
      continue;
    }
    if (nl) {
      if (s[i].dns == D_NONE) {
        char name[NAME];
        memcpy(name, f + 7, nl);
        name[nl] = 0;
        if (!resolve(i, name)) s[i].dns = D_FAILED;
      }
      if (s[i].dns == D_NONE || s[i].dns == D_WAIT) {
        if (!s[i].waiting) {
          s[i].waiting = 1;
          s[i].deadline = now + RESOLVE_MS;
        }
        if (!expired(i, now)) break;
        // Give up on this datagram; a late answer must not pass for the next one's.
        if (s[i].ask) asks[s[i].ask - 1].slot = -1;
        s[i].ask = 0;
        s[i].dns = D_FAILED;
      }
      int ok = s[i].dns == D_OK;
      s[i].dns = D_NONE;
      s[i].waiting = 0;
      if (!ok) {
        tail += body + 2;
        continue;
      }
      to = s[i].addr;
    } else {
      IP4_ADDR(ip_2_ip4(&to), ip >> 24, ip >> 16 & 255, ip >> 8 & 255, ip & 255);
    }
    tail += body + 2;
    unsigned n = body - 7 - nl;
    struct pbuf *p = pbuf_alloc(PBUF_TRANSPORT, n, PBUF_RAM);
    if (!p) continue;
    memcpy(p->payload, f + 7 + nl, n);
    udp_sendto(s[i].udp, p, &to, port);
    pbuf_free(p);
  }
  q->tx_tail = tail;
}

// End a slot's use and empty Radio's side of its rings for the next. Only
// here: the Open Runtime may have given the slot to another app before Radio
// saw the close, and until this runs the old connection's callbacks still
// write the receive ring. After drop() none can, so nothing of the old
// connection is in the new one's ring.
static void renew(int i) {
  volatile struct slot *q = slot(i);
  drop(i);
  q->tx_tail = 0;
  q->rx_head = 0;
  q->local_port = 0;
  q->local_ip = 0;
  barrier();
}

static void command(int i, unsigned online, unsigned now) {
  volatile struct slot *q = slot(i);
  unsigned cmd = q->cmd;
  renew(i);
  s[i].owner = q->owner;
  if (cmd == CMD_CLOSE) {
    q->state = IDLE;
    return;
  }
  q->state = CONNECTING;
  q->error = 0;
  if (cmd == CMD_UDP) {
    if (online == 2 || !udp_start(i)) return fail(i, E_NETUNREACH);
    s[i].phase = P_UP;
    q->state = OPEN;
    return;
  }
  if (cmd != CMD_CONNECT) return fail(i, E_INVALID);
  if (q->name[0]) {
    char name[NAME];
    slot_name(i, name);
    if (numeric(name)) return fail(i, E_NONAME);
  }
  if (online == 2) {
    // SOCKS5 CONNECT through Violet Relay on the phone.
    unsigned char password[64];
    unsigned plen = 0;
    ip_addr_t phone;
    IP4_ADDR(ip_2_ip4(&phone), 192, 168, 77, 2);
    if (!station_relay_info(password, &plen)) return fail(i, E_NETUNREACH);
    s[i].relay = 1;
    return start_tcp(i, &phone, 1081, now);
  }
  if (q->name[0]) {
    s[i].phase = P_RESOLVE;
    s[i].deadline = now + RESOLVE_MS;
    return advance(i, now);
  }
  unsigned ip = q->ip;
  ip_addr_t addr;
  IP4_ADDR(ip_2_ip4(&addr), ip >> 24, ip >> 16 & 255, ip >> 8 & 255, ip & 255);
  start_tcp(i, &addr, q->port, now);
}

// A TCP slot that is sending: bytes wait in its ring or in lwIP.
static int sending(int i) {
  return s[i].phase == P_UP && s[i].pcb &&
         (slot(i)->tx_head != slot(i)->tx_tail || tcp_sndbuf(s[i].pcb) < TCP_SND_BUF);
}

static void pump(int i, unsigned now) {
  volatile struct slot *q = slot(i);
  if (s[i].phase != P_UP) return;
  if (s[i].udp) return pump_udp(i, now);
  if (!s[i].pcb) return fail(i, E_RESET);
  // Bytes from the app to the network, up to this slot's share of lwIP's heap.
  unsigned head = q->tx_head, tail = q->tx_tail;
  if (superseded(i)) return;
  if (head - tail > TX) return fail(i, E_RESET);
  while (head != tail) {
    unsigned n = head - tail, at = tail % TX, room = tcp_sndbuf(s[i].pcb);
    unsigned queued = room < TCP_SND_BUF ? TCP_SND_BUF - room : 0;
    if (queued >= share) break;
    if (n > TX - at) n = TX - at;
    if (n > room) n = room;
    if (n > share - queued) n = share - queued;
    if (!n || tcp_write(s[i].pcb, tx_ring(i) + at, n, TCP_WRITE_FLAG_COPY) != ERR_OK) break;
    tail += n;
  }
  if (tail != q->tx_tail) {
    q->tx_tail = tail;
    tcp_output(s[i].pcb);
  }
  // Word 11 is the Open Runtime's: that it was done is kept here.
  if (q->shut == 1 && tail == head && !s[i].shut) {
    tcp_shutdown(s[i].pcb, 0, 1);
    s[i].shut = 1;
  }
  // Bytes from the network are in the ring already (on_received).
  credit(i);
  if (s[i].eof && q->state == OPEN) q->state = PEER_CLOSED;
}

// Called from Radio's event loop. `online` is 0, 1 (joined network) or 2 (relay).
void open_sockets_poll(void *base, unsigned online) {
  page = base;
  unsigned now = radio_monotonic_ms();
  if (!count) count = fitting(page[5]);
  if (page[0] != MAGIC || page[2] != count) {
    unsigned extent = page[5];
    page[0] = 0;
    memset((void *)page, 0, HEADERS + SLOTS * HEADER);
    page[5] = extent;
    page[2] = count;
    page[3] = TX;
    page[4] = RX;
    page[0] = MAGIC;
  }
  if (page[1] != online) {
    page[1] = online;
    if (!online)
      for (int i = 0; i < (int)count; i++) {
        drop(i);
        if (slot(i)->state != IDLE) {
          slot(i)->state = FAILED;
          slot(i)->error = E_UNREACH;
        }
      }
  }
  // Half of lwIP's heap, split between the slots that are sending: one whose
  // peer has gone quiet cannot hold what the others need.
  unsigned senders = 0;
  for (int i = 0; i < (int)count; i++) senders += sending(i);
  share = MEM_SIZE / 2 / (senders ? senders : 1);
  if (share < MIN_SHARE) share = MIN_SHARE;
  for (int i = 0; i < (int)count; i++) {
    volatile struct slot *q = slot(i);
    unsigned seq = q->cmd_seq;
    if (seq != q->done_seq) {
      if (online) command(i, online, now);
      else {
        renew(i);
        q->state = FAILED;
        q->error = E_UNREACH;
      }
      // Taken: the state stays CONNECTING until the connect is through.
      q->done_seq = seq;
    }
    if (!online) continue;
    if (s[i].phase == P_IDLE) {
      // A connection this Radio does not have (it restarted): reset, not silence.
      unsigned state = q->state;
      if (state == CONNECTING || state == OPEN || state == PEER_CLOSED) fail(i, E_RESET);
    } else if (s[i].phase != P_UP) {
      advance(i, now);
    } else {
      pump(i, now);
    }
  }
  if (online) sys_check_timeouts();
}
