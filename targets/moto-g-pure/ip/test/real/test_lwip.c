// Host test of ../../sockets.c against lwIP itself (make test-net-lwip; local,
// it needs lwIP's source, which the image build downloads). test_sockets.c
// drives sockets.c through a stand-in for lwIP; this one links the real
// tcp.c, dns.c and the rest, with Radio's lwipopts.h, and plays the network
// by hand: a netif whose output the test reads, and a peer that answers
// every SYN, acknowledges data and FINs, and never closes.
//
// It is here for the claims that rest on what lwIP does, not on what
// sockets.c does: that lwIP gives a closing connection up to a new one, that
// it calls back for every question it was asked, and what it reads as an
// address.
#include "../../sockets.c"
#include "lwip/init.h"
#include "lwip/ip4.h"
#include "lwip/priv/tcp_priv.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>

static unsigned now_ms = 1000, seed = 1;
unsigned radio_monotonic_ms(void) { return now_ms; }
u32_t sys_now(void) { return now_ms; }
unsigned radio_ip_random(void) { return seed = seed * 1103515245 + 12345; }
int station_relay_info(unsigned char *password, unsigned *len) { return 0; }

// ---- The network ------------------------------------------------------------
static struct netif nif;
static struct packet {
  unsigned len;
  unsigned char b[1600];
} out[256];
static unsigned out_count;
static err_t wire(struct netif *n, struct pbuf *p, const ip4_addr_t *to) {
  assert(out_count < 256 && p->tot_len <= sizeof out[0].b);
  out[out_count].len = p->tot_len;
  pbuf_copy_partial(p, out[out_count].b, p->tot_len, 0);
  out_count++;
  return ERR_OK;
}
static err_t nif_init(struct netif *n) {
  n->output = wire;
  n->mtu = 1500;
  n->name[0] = 't';
  n->name[1] = 'n';
  return ERR_OK;
}
static unsigned be16(const unsigned char *b) { return b[0] << 8 | b[1]; }
static unsigned be32(const unsigned char *b) { return (unsigned)b[0] << 24 | b[1] << 16 | b[2] << 8 | b[3]; }
static void put16(unsigned char *b, unsigned v) { b[0] = v >> 8; b[1] = v; }
static void put32(unsigned char *b, unsigned v) { b[0] = v >> 24; b[1] = v >> 16; b[2] = v >> 8; b[3] = v; }
// Hand lwIP an IPv4 packet from `from` to this phone.
static void inject(unsigned from, unsigned proto, const unsigned char *body, unsigned n) {
  unsigned char b[1600] = {0x45, 0};
  put16(b + 2, 20 + n);
  b[8] = 64;
  b[9] = proto;
  put32(b + 12, from);
  put32(b + 16, 0x0a000002);
  memcpy(b + 20, body, n);
  struct pbuf *p = pbuf_alloc(PBUF_RAW, 20 + n, PBUF_POOL);
  assert(p && pbuf_take(p, b, 20 + n) == ERR_OK);
  if (ip4_input(p, &nif) != ERR_OK) pbuf_free(p);
}

// The peer's side of each connection, by this phone's port.
static struct conn {
  unsigned port, host, theirs, mine, fins;
} conns[512];
static unsigned conn_count, syns, queries, answer_names;
static char asked_names[512][64];
static struct conn *conn_of(unsigned port, unsigned host) {
  for (unsigned k = 0; k < conn_count; k++)
    if (conns[k].port == port && conns[k].host == host) return &conns[k];
  return NULL;
}
static void segment(struct conn *c, unsigned flags, const void *data, unsigned n) {
  unsigned char t[1500] = {0};
  put16(t, 80);
  put16(t + 2, c->port);
  put32(t + 4, c->mine);
  put32(t + 8, c->theirs);
  t[12] = 5 << 4;
  t[13] = flags;
  put16(t + 14, 65535);
  memcpy(t + 20, data, n);
  c->mine += n + (flags & 2 ? 1 : 0);
  inject(c->host, 6, t, 20 + n);
}
static int was_asked(const char *name) {
  for (unsigned k = 0; k < queries; k++)
    if (!strcmp(asked_names[k], name)) return 1;
  return 0;
}
// What the network does with everything this phone sent.
static void network(void) {
  while (out_count) {
    static struct packet taken[256];
    unsigned n = out_count;
    memcpy(taken, out, n * sizeof *out);
    out_count = 0;
    for (unsigned k = 0; k < n; k++) {
      const unsigned char *b = taken[k].b, *t = b + (b[0] & 15) * 4;
      unsigned host = be32(b + 16), body = taken[k].len - (b[0] & 15) * 4;
      if (b[9] == 17 && be16(t + 2) == 53) {
        // A DNS question: remembered; answered only for names under "ok.".
        char name[64] = "";
        const unsigned char *q = t + 8 + 12;
        while (*q && strlen(name) + *q + 2 < sizeof name) {
          if (name[0]) strcat(name, ".");
          strncat(name, (const char *)q + 1, *q);
          q += *q + 1;
        }
        assert(queries < 512);
        strcpy(asked_names[queries++], name);
        if (answer_names && !strncmp(name, "ok.", 3)) {
          unsigned char r[600];
          unsigned qlen = body - 8;
          put16(r, 53);
          put16(r + 2, be16(t));
          put16(r + 4, 8 + qlen + 16);
          put16(r + 6, 0);
          memcpy(r + 8, t + 8, qlen);
          r[8 + 2] = 0x81;
          r[8 + 3] = 0x80;
          put16(r + 8 + 6, 1);
          static const unsigned char answer[16] = {0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 203, 0, 113, 77};
          memcpy(r + 8 + qlen, answer, 16);
          inject(host, 17, r, 8 + qlen + 16);
        }
        continue;
      }
      if (b[9] != 6) continue;
      unsigned port = be16(t), seq = be32(t + 4), flags = t[13], data = body - (t[12] >> 4) * 4;
      struct conn *c = conn_of(port, host);
      if (flags & 4) continue;  // RST
      if (flags & 2) {
        syns++;
        if (!c) {
          assert(conn_count < 512);
          c = &conns[conn_count++];
        }
        *c = (struct conn){port, host, seq + 1, 7000 + 1000 * conn_count, 0};
        segment(c, 0x12, NULL, 0);
        continue;
      }
      if (!c) continue;
      if (data || flags & 1) {
        if (seq == c->theirs) c->theirs += data + (flags & 1);
        c->fins += flags & 1;
        segment(c, 0x10, NULL, 0);  // acknowledged; the peer never closes
      }
    }
  }
}

// ---- The Open Runtime's side (as test_sockets.c) -----------------------------
static unsigned char *region;
static unsigned as_owner;
static void step(unsigned ms) {
  now_ms += ms;
  open_sockets_poll(region, 1);
  network();
}
static void start(int i, unsigned cmd, unsigned ip, unsigned port, const char *name) {
  volatile struct slot *q = slot(i);
  memset((void *)q->name, 0, NAME);
  if (name) strcpy((char *)q->name, name);
  q->ip = ip;
  q->port = port;
  q->tx_head = q->rx_tail = q->shut = 0;
  q->owner = as_owner;
  q->state = CONNECTING;
  q->error = 0;
  q->cmd = cmd;
  q->cmd_seq = q->cmd_seq + 1;
}
static int taken(int i) { return slot(i)->cmd_seq == slot(i)->done_seq; }
static unsigned state(int i) { return taken(i) ? slot(i)->state : CONNECTING; }
static unsigned ready(int i) { return taken(i) ? slot(i)->rx_head - slot(i)->rx_tail : 0; }
static unsigned ip4(int a, int b, int c, int d) { return (unsigned)a << 24 | b << 16 | c << 8 | d; }
static int opens(int i) {
  for (int k = 0; k < 40 && state(i) == CONNECTING; k++) step(5);
  return state(i) == OPEN;
}
static unsigned pcbs_in(enum tcp_state st) {
  unsigned n = 0;
  for (struct tcp_pcb *p = tcp_active_pcbs; p; p = p->next) n += p->state == st;
  return n;
}
static unsigned own_questions(unsigned owner) {
  unsigned n = 0;
  for (int k = 0; k < ASKS; k++) n += asks[k].used && asks[k].owner == owner;
  return n;
}
#define RUN(test) (test(), printf("ok   %s\n", #test))

static void closing_connections_give_way_to_new_ones(void) {
  // An app in its share of the slots, against a server that never closes.
  for (int round = 0; round < 100; round++) {
    start(0, CMD_CONNECT, ip4(203, 0, 113, 5), 80, NULL);
    if (!opens(0)) {
      printf("round %d: state %u error %u, %u connections half closed\n", round, state(0), slot(0)->error, pcbs_in(FIN_WAIT_2));
      assert(!"a connect found no connection in lwIP");
    }
    start(0, CMD_CLOSE, 0, 0, NULL);
    step(5);
    step(5);
  }
  // The closed connections are really still there (the test's premise), ...
  assert(now_ms < 20000 && pcbs_in(FIN_WAIT_2) >= 5);
  // ... and every slot connects all the same, the last ones included.
  for (int i = 0; i < SLOTS; i++) start(i, CMD_CONNECT, ip4(203, 0, 113, 6), 80, NULL);
  for (int i = 0; i < SLOTS; i++) assert(opens(i));
  assert(pcbs_in(ESTABLISHED) == SLOTS);
  for (int i = 0; i < SLOTS; i++) start(i, CMD_CLOSE, 0, 0, NULL);
  step(5);
}

static void a_reused_slot_starts_empty(void) {
  start(3, CMD_CONNECT, ip4(203, 0, 113, 7), 80, NULL);
  assert(opens(3));
  struct conn *old = conn_of(s[3].pcb->local_port, ip4(203, 0, 113, 7));
  assert(old);
  segment(old, 0x18, "read by A", 9);
  assert(ready(3) == 9);
  // A closes, B gets the slot, and A's server sends on before Radio's next poll.
  start(3, CMD_CLOSE, 0, 0, NULL);
  start(3, CMD_CONNECT, ip4(203, 0, 113, 8), 80, NULL);
  segment(old, 0x18, "A's secret bytes", 16);
  assert(slot(3)->rx_head == 25);  // lwIP did deliver them to the old connection
  assert(ready(3) == 0);
  assert(opens(3));
  assert(slot(3)->rx_head == 0 && ready(3) == 0);
  struct conn *new = conn_of(s[3].pcb->local_port, ip4(203, 0, 113, 8));
  segment(new, 0x18, "for B", 5);
  assert(ready(3) == 5 && !memcmp(rx_ring(3), "for B", 5));
  // More of A's stream, later: lwIP has no connection for it any more.
  segment(old, 0x18, "more", 4);
  step(5);
  assert(ready(3) == 5);
  start(3, CMD_CLOSE, 0, 0, NULL);
  step(5);
}

static void a_name_that_is_an_address_is_refused(void) {
  static const char *const forms[] = {"10.0.0.9", "167772169", "0x0a000009", "0X0A000009", "10.9", "10.0.9", "012.0.0.011", "0", "0x"};
  unsigned before_syns = syns, before_queries = queries;
  for (unsigned k = 0; k < sizeof forms / sizeof *forms; k++) {
    // lwIP itself would take each for an address, without asking anyone.
    ip_addr_t a;
    assert(dns_gethostbyname(forms[k], &a, NULL, NULL) == ERR_OK);
    start(k, CMD_CONNECT, 0, 443, forms[k]);
  }
  for (int k = 0; k < 10; k++) step(5);
  for (unsigned k = 0; k < sizeof forms / sizeof *forms; k++) assert(state(k) == FAILED && slot(k)->error == E_NONAME);
  assert(syns == before_syns && queries == before_queries);
}

static void one_app_cannot_fill_the_name_table(void) {
  char name[64];
  unsigned serial = 0;
  as_owner = 7;
  for (int round = 0; round < 60; round++) {
    for (int i = 0; i < 15; i++) {
      snprintf(name, sizeof name, "h%u.hog.example", serial++);
      start(i, CMD_CONNECT, 0, 80, name);
    }
    step(20);
    assert(own_questions(7) == ASKS_EACH);
  }
  // Another app's name and the Open Runtime's own go out at once and resolve.
  answer_names = 1;
  as_owner = 8;
  start(20, CMD_CONNECT, 0, 80, "ok.peer.example");
  as_owner = 0;
  start(31, CMD_CONNECT, 0, 80, "ok.sentinel.example");
  step(5);
  assert(was_asked("ok.peer.example") && was_asked("ok.sentinel.example"));
  assert(opens(20) && opens(31));
  assert(conn_of(s[20].pcb->local_port, ip4(203, 0, 113, 77)));
  // lwIP gives the hog's questions up in its own time and says so each time,
  // whatever became of the slots: none is counted for ever.
  as_owner = 7;
  for (int i = 0; i < 15; i++) start(i, CMD_CLOSE, 0, 0, NULL);
  for (int k = 0; k < 80 && own_questions(7); k++) step(250);
  assert(own_questions(7) == 0 && now_ms < 40000);
  unsigned hogs = 0;
  for (unsigned k = 0; k < queries; k++) hogs += strstr(asked_names[k], ".hog.example") != NULL;
  assert(hogs >= ASKS_EACH);
  // Then it may ask again.
  start(0, CMD_CONNECT, 0, 80, "ok.hog.example");
  assert(opens(0));
  as_owner = 0;
}

int main(void) {
  lwip_init();
  ip4_addr_t ip, mask, gw;
  IP4_ADDR(&ip, 10, 0, 0, 2);
  IP4_ADDR(&mask, 255, 255, 255, 0);
  IP4_ADDR(&gw, 10, 0, 0, 1);
  assert(netif_add(&nif, &ip, &mask, &gw, NULL, nif_init, ip4_input));
  netif_set_default(&nif);
  netif_set_up(&nif);
  netif_set_link_up(&nif);
  dns_setserver(0, &gw);
  region = calloc(1, 0x100000);
  ((unsigned *)region)[5] = 0xd0000;
  step(5);
  assert(((unsigned *)region)[2] == SLOTS);
  RUN(closing_connections_give_way_to_new_ones);
  RUN(a_reused_slot_starts_empty);
  RUN(a_name_that_is_an_address_is_refused);
  RUN(one_app_cannot_fill_the_name_table);
  puts("sockets.c with lwIP: all tests passed");
  return 0;
}
