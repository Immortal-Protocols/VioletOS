// Host test of ../sockets.c (make test-unit): the real file, compiled against
// a small stand-in for lwIP (fake_lwip.h) that the test drives by hand. The
// clock only moves when a test moves it and nothing here can poll the
// network, so a sockets.c that waited for anything would hang the test.
#include "../sockets.c"
#include <arpa/inet.h>
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>

// ---- The stand-in for lwIP -------------------------------------------------
const ip_addr_t fake_any;
struct netif *netif_default;
static unsigned now_ms, live_pbufs, live_tcp, live_udp, refuse_writes, no_pcbs;
unsigned radio_monotonic_ms(void) { return now_ms; }
static int relay_known = 1;
int station_relay_info(unsigned char *password, unsigned *len) {
  memcpy(password, "secret-pass", 11);
  *len = 11;
  return relay_known;
}
void sys_check_timeouts(void) {}

struct pbuf *pbuf_alloc(int layer, u16_t length, int type) {
  struct pbuf *p = calloc(1, sizeof *p + length);
  p->payload = p + 1;
  p->tot_len = p->len = length;
  live_pbufs++;
  return p;
}
unsigned char pbuf_free(struct pbuf *p) {
  while (p) {
    struct pbuf *next = p->next;
    free(p);
    live_pbufs--;
    p = next;
  }
  return 1;
}

int ipaddr_aton(const char *cp, ip_addr_t *addr) {
  struct in_addr in;
  if (!inet_aton(cp, &in)) return 0;
  addr->addr = in.s_addr;
  return 1;
}

// Every pcb made, so a test can find the one a slot opened. lwIP has
// MEMP_NUM_TCP_PCB of them: a connection closed gracefully keeps its pcb
// (`lingers`) for as long as its peer does not close, and when none is free
// tcp_new takes the oldest one whose priority is below the new connection's
// (lwIP's tcp_alloc and tcp_kill_prio), or fails.
static struct tcp_pcb *made[4096];
static unsigned made_count, lingering;
struct tcp_pcb *tcp_new(void) {
  if (no_pcbs) return NULL;
  if (live_tcp + lingering >= MEMP_NUM_TCP_PCB) {
    struct tcp_pcb *oldest = NULL;
    for (unsigned k = 0; k < made_count && !oldest; k++)
      if (made[k]->lingers && made[k]->prio < TCP_PRIO_NORMAL) oldest = made[k];
    if (!oldest) return NULL;
    oldest->lingers = 0;
    oldest->aborted = 1;
    lingering--;
  }
  struct tcp_pcb *pcb = calloc(1, sizeof *pcb);
  pcb->snd_buf = TCP_SND_BUF;
  pcb->rcv_wnd = TCP_WND;
  pcb->prio = TCP_PRIO_NORMAL;
  assert(made_count < 4096);
  made[made_count++] = pcb;
  live_tcp++;
  return pcb;
}
void tcp_setprio(struct tcp_pcb *pcb, unsigned char prio) { pcb->prio = prio; }
void tcp_arg(struct tcp_pcb *pcb, void *arg) { pcb->arg = arg; }
void tcp_recv(struct tcp_pcb *pcb, tcp_recv_fn fn) { pcb->recv = fn; }
void tcp_err(struct tcp_pcb *pcb, tcp_err_fn fn) { pcb->err = fn; }
err_t tcp_connect(struct tcp_pcb *pcb, const ip_addr_t *addr, u16_t port, tcp_connected_fn fn) {
  pcb->remote = *addr;
  pcb->port = port;
  pcb->connected = fn;
  return ERR_OK;
}
err_t tcp_write(struct tcp_pcb *pcb, const void *data, u16_t len, unsigned char flags) {
  if (refuse_writes || len > pcb->snd_buf) return ERR_MEM;
  assert(pcb->out_len + len <= sizeof pcb->out);
  memcpy(pcb->out + pcb->out_len, data, len);
  pcb->out_len += len;
  pcb->snd_buf -= len;
  pcb->unsent = pcb;
  return ERR_OK;
}
err_t tcp_output(struct tcp_pcb *pcb) { return ERR_OK; }
static unsigned peers_never_close;
err_t tcp_close(struct tcp_pcb *pcb) {
  pcb->closed = 1;
  live_tcp--;
  if (peers_never_close) {
    pcb->lingers = 1;
    lingering++;
  }
  return ERR_OK;
}
void tcp_abort(struct tcp_pcb *pcb) {
  pcb->aborted = 1;
  live_tcp--;
}
err_t tcp_shutdown(struct tcp_pcb *pcb, int rx, int tx) {
  pcb->shut = 1;
  return ERR_OK;
}
void tcp_recved(struct tcp_pcb *pcb, u16_t len) {
  pcb->rcv_wnd += len;
  assert(pcb->rcv_wnd <= TCP_WND);
}
unsigned tcp_sndbuf(struct tcp_pcb *pcb) { return pcb->snd_buf; }

static struct {
  ip_addr_t to;
  unsigned port, len, count;
  unsigned char data[2048];
} sent_udp;
static unsigned next_port = 40000;
struct udp_pcb *udp_new(void) {
  live_udp++;
  return calloc(1, sizeof(struct udp_pcb));
}
err_t udp_bind(struct udp_pcb *pcb, const ip_addr_t *addr, u16_t port) {
  pcb->local_port = next_port++;
  return ERR_OK;
}
void udp_recv(struct udp_pcb *pcb, udp_recv_fn fn, void *arg) {
  pcb->recv = fn;
  pcb->arg = arg;
}
err_t udp_sendto(struct udp_pcb *pcb, struct pbuf *p, const ip_addr_t *addr, u16_t port) {
  sent_udp.to = *addr;
  sent_udp.port = port;
  sent_udp.len = p->tot_len;
  memcpy(sent_udp.data, p->payload, p->tot_len);
  sent_udp.count++;
  return ERR_OK;
}
void udp_remove(struct udp_pcb *pcb) {
  live_udp--;
  free(pcb);
}

// Names starting "now." resolve at once, "full." find lwIP's table full,
// "bad." cannot be asked; any other waits for dns_answer. As in lwIP, a
// question stays until it is answered (there is no cancelling one), and only
// DNS_TABLE_SIZE can be out at once.
static struct {
  char name[NAME];
  dns_found_callback found;
  void *arg;
  ip_addr_t *into;
} asked[2048];
static unsigned asked_count, full_for, table_full;
static unsigned dns_out(void) {
  unsigned n = 0;
  for (unsigned k = 0; k < asked_count; k++) n += asked[k].found != NULL;
  return n;
}
err_t dns_gethostbyname(const char *name, ip_addr_t *addr, dns_found_callback found, void *arg) {
  if (!strncmp(name, "now.", 4)) {
    IP4_ADDR(addr, 10, 1, 1, 1);
    return ERR_OK;
  }
  if (!strncmp(name, "bad.", 4)) return ERR_ARG;
  if (!strncmp(name, "full.", 5) && full_for) {
    full_for--;
    return ERR_MEM;
  }
  if (dns_out() >= DNS_TABLE_SIZE) {
    table_full++;
    return ERR_MEM;
  }
  assert(asked_count < 2048 && strlen(name) < NAME);
  strcpy(asked[asked_count].name, name);
  asked[asked_count].found = found;
  asked[asked_count].arg = arg;
  asked_count++;
  return ERR_INPROGRESS;
}
// Answer every question asked for `name`: with an address, or with none.
static unsigned dns_answer(const char *name, int a, int b, int c, int d) {
  unsigned n = 0;
  for (unsigned k = 0; k < asked_count; k++) {
    if (strcmp(asked[k].name, name) || !asked[k].found) continue;
    ip_addr_t addr;
    IP4_ADDR(&addr, a, b, c, d);
    dns_found_callback found = asked[k].found;
    asked[k].found = NULL;
    found(name, a < 0 ? NULL : &addr, asked[k].arg);
    n++;
  }
  return n;
}

// ---- The Open Runtime's side of the pages (as runtime/open/net.rs) ----------
static unsigned char *region;
static unsigned online = 1;
enum { REGION = 0x100000, HOLE = 0x40000, HOLE_END = 0x43000, END = 0xcd000, MAPPED = 0xd0000 };
// What the supervisor says it mapped (page 0, word 5), and whose socket the
// next start() is (a slot header's word 14).
static unsigned extent = MAPPED, as_owner;
static void poll(void) { open_sockets_poll(region, online); }
static void fresh(void) {
  for (int i = 0; i < SLOTS; i++) drop(i);
  memset(s, 0, sizeof s);
  memset(asks, 0, sizeof asks);
  count = 0;
  free(region);
  region = calloc(1, REGION);
  ((unsigned *)region)[5] = extent;
  memset(region + HOLE, 0xa5, HOLE_END - HOLE);
  memset(region + END, 0x5a, REGION - END);
  online = 1;
  now_ms = 1000;
  made_count = asked_count = full_for = refuse_writes = no_pcbs = 0;
  live_tcp = live_udp = lingering = peers_never_close = table_full = as_owner = 0;
  relay_known = 1;
  memset(&sent_udp, 0, sizeof sent_udp);
  netif_default = NULL;
  poll();
}
static void start(int i, unsigned cmd, unsigned ip, unsigned port, const char *name) {
  volatile struct slot *q = slot(i);
  memset((void *)q->name, 0, NAME);
  if (name) strcpy((char *)q->name, name);
  q->ip = ip;
  q->port = port;
  // Its own ring words only: Radio empties the others when it takes the command.
  q->tx_head = q->rx_tail = q->shut = 0;
  q->owner = as_owner;
  q->state = CONNECTING;
  q->error = 0;
  q->cmd = cmd;
  q->cmd_seq = q->cmd_seq + 1;
}
static int taken(int i) { return slot(i)->cmd_seq == slot(i)->done_seq; }
static unsigned state(int i) { return taken(i) ? slot(i)->state : CONNECTING; }
static void app_send(int i, const void *data, unsigned n) {
  volatile struct slot *q = slot(i);
  assert(TX - (q->tx_head - (taken(i) ? q->tx_tail : 0)) >= n);
  for (unsigned k = 0; k < n; k++) tx_ring(i)[(q->tx_head + k) % TX] = ((const unsigned char *)data)[k];
  q->tx_head = q->tx_head + n;
}
static unsigned app_read(int i, unsigned char *out, unsigned max) {
  volatile struct slot *q = slot(i);
  // Until Radio takes the command the ring is the slot's previous use's.
  unsigned n = taken(i) ? q->rx_head - q->rx_tail : 0;
  if (n > max) n = max;
  for (unsigned k = 0; k < n; k++) out[k] = rx_ring(i)[(q->rx_tail + k) % RX];
  q->rx_tail = q->rx_tail + n;
  return n;
}
// The network's side: finish a connect, deliver bytes (in two pbufs, within
// the window lwIP advertised), close.
static struct tcp_pcb *pcb_of(int i) { return s[i].pcb; }
static void net_connected(struct tcp_pcb *pcb) { pcb->connected(pcb->arg, pcb, ERR_OK); }
static err_t net_deliver(struct tcp_pcb *pcb, const void *data, unsigned n) {
  unsigned a = n / 2, b = n - a;
  struct pbuf *p = pbuf_alloc(PBUF_TRANSPORT, a, PBUF_RAM), *rest = pbuf_alloc(PBUF_TRANSPORT, b, PBUF_RAM);
  memcpy(p->payload, data, a);
  memcpy(rest->payload, (const unsigned char *)data + a, b);
  p->next = rest;
  p->tot_len = n;
  err_t rc = pcb->recv(pcb->arg, pcb, p, ERR_OK);
  if (rc == ERR_OK) pcb->rcv_wnd -= n;
  else pbuf_free(p);  // lwIP would keep it and offer it again
  return rc;
}
static void net_ack(struct tcp_pcb *pcb) {
  pcb->snd_buf = TCP_SND_BUF;
  pcb->unsent = pcb->unacked = NULL;
}
static unsigned ip4(int a, int b, int c, int d) { return (unsigned)a << 24 | b << 16 | c << 8 | d; }
static unsigned remote(struct tcp_pcb *pcb) { return __builtin_bswap32(pcb->remote.addr); }
static void open_tcp(int i) {
  start(i, CMD_CONNECT, ip4(93, 184, 216, 34), 443, NULL);
  poll();
  net_connected(pcb_of(i));
  poll();
  assert(state(i) == OPEN);
}

#define RUN(test) (test(), printf("ok   %s\n", #test))

static void layout_is_net2_and_keeps_out_of_the_hole(void) {
  fresh();
  unsigned *page0 = (unsigned *)region;
  assert(page0[0] == 0x3254454e && page0[2] == 32 && page0[3] == 8192 && page0[4] == 16384);
  assert((unsigned char *)slot(0) == region + 0x1000 && (unsigned char *)slot(31) + 512 == region + 0x5000);
  assert(tx_ring(0) == region + 0x5000 && rx_ring(8) + RX <= region + HOLE);
  assert(tx_ring(9) == region + HOLE_END && rx_ring(31) + RX == region + END);
  for (int i = 1; i < SLOTS; i++) assert(tx_ring(i) >= rx_ring(i - 1) + RX);
  // Every slot connects at once and moves a ring's worth each way, twice.
  unsigned char data[RX], back[RX];
  for (int i = 0; i < SLOTS; i++) open_tcp(i);
  assert(live_tcp == SLOTS);
  for (int round = 0; round < 2; round++)
    for (int i = 0; i < SLOTS; i++) {
      for (unsigned k = 0; k < sizeof data; k++) data[k] = k * 7 + i + round;
      for (unsigned done = 0; done < TX;) {
        app_send(i, data + done, 2048);
        poll();
        net_ack(pcb_of(i));
        done += 2048;
      }
      assert(slot(i)->tx_tail == slot(i)->tx_head);
      assert(!memcmp(pcb_of(i)->out + round * TX, data, TX));
      for (unsigned done = 0; done < RX;) {
        assert(net_deliver(pcb_of(i), data + done, 4096) == ERR_OK);
        poll();
        assert(app_read(i, back + done, 4096) == 4096);
        poll();
        done += 4096;
      }
      assert(!memcmp(back, data, RX));
    }
  for (unsigned k = HOLE; k < HOLE_END; k++) assert(region[k] == 0xa5);
  for (unsigned k = END; k < REGION; k++) assert(region[k] == 0x5a);
  assert(live_pbufs == 0);
}

static void a_connect_that_never_answers_delays_nobody(void) {
  fresh();
  start(0, CMD_CONNECT, ip4(203, 0, 113, 1), 80, NULL);  // a black hole
  poll();
  // The command is taken at once; the slot is still connecting.
  assert(slot(0)->done_seq == slot(0)->cmd_seq && slot(0)->state == CONNECTING);
  assert(remote(pcb_of(0)) == ip4(203, 0, 113, 1) && pcb_of(0)->port == 80);
  // Meanwhile another slot connects and talks, poll after poll.
  open_tcp(1);
  unsigned char buf[64];
  for (int round = 0; round < 100; round++) {
    now_ms += 50;
    app_send(1, "ping", 4);
    poll();
    assert(pcb_of(1)->out_len == 4u * (round + 1));
    net_ack(pcb_of(1));
    assert(net_deliver(pcb_of(1), "pong!", 5) == ERR_OK);
    assert(app_read(1, buf, sizeof buf) == 5 && !memcmp(buf, "pong!", 5));
    assert(state(0) == CONNECTING);
  }
  // 5 s in: not yet. At 8 s the black hole times out, and only it.
  now_ms = 1000 + CONNECT_MS - 1;
  poll();
  assert(state(0) == CONNECTING);
  struct tcp_pcb *hole = pcb_of(0);
  now_ms++;
  poll();
  assert(state(0) == FAILED && slot(0)->error == E_TIMEOUT && (hole->closed || hole->aborted) && !pcb_of(0));
  assert(state(1) == OPEN);
  // Refused: the error callback, as lwIP reports a reset.
  start(0, CMD_CONNECT, ip4(203, 0, 113, 2), 81, NULL);
  poll();
  pcb_of(0)->err(pcb_of(0)->arg, ERR_RST);
  poll();
  assert(state(0) == FAILED && slot(0)->error == E_REFUSED);
  // No pcb to be had.
  no_pcbs = 1;
  start(0, CMD_CONNECT, ip4(203, 0, 113, 2), 81, NULL);
  poll();
  assert(state(0) == FAILED && slot(0)->error == E_NOBUFS);
  start(0, 9, 0, 0, NULL);
  poll();
  assert(state(0) == FAILED && slot(0)->error == E_INVALID);
}

static void names_resolve_without_waiting(void) {
  fresh();
  start(0, CMD_CONNECT, 0, 443, "slow.example");
  start(1, CMD_CONNECT, 0, 443, "now.example");
  poll();
  // The slow name is asked for; the quick one is already connecting.
  assert(asked_count == 1 && state(0) == CONNECTING && !pcb_of(0));
  assert(pcb_of(1) && remote(pcb_of(1)) == ip4(10, 1, 1, 1) && pcb_of(1)->port == 443);
  net_connected(pcb_of(1));
  poll();
  assert(state(1) == OPEN);
  assert(dns_answer("slow.example", 10, 2, 2, 2) == 1);
  poll();
  assert(remote(pcb_of(0)) == ip4(10, 2, 2, 2));
  net_connected(pcb_of(0));
  poll();
  assert(state(0) == OPEN);
  // No such name; a name that cannot be asked; a name nobody answers.
  start(2, CMD_CONNECT, 0, 80, "gone.example");
  start(3, CMD_CONNECT, 0, 80, "bad.example");
  start(4, CMD_CONNECT, 0, 80, "silent.example");
  poll();
  assert(state(3) == FAILED && slot(3)->error == E_NONAME);
  dns_answer("gone.example", -1, 0, 0, 0);
  poll();
  assert(state(2) == FAILED && slot(2)->error == E_NONAME);
  now_ms += RESOLVE_MS - 1;
  poll();
  assert(state(4) == CONNECTING);
  now_ms++;
  poll();
  assert(state(4) == FAILED && slot(4)->error == E_NONAME && state(0) == OPEN && state(1) == OPEN);
  // lwIP's table of questions is full for two polls: asked again, then answered.
  full_for = 2;
  start(5, CMD_CONNECT, 0, 80, "full.example");
  poll();
  poll();
  assert(state(5) == CONNECTING && !pcb_of(5) && full_for == 0);
  poll();
  assert(dns_answer("full.example", 10, 5, 5, 5) == 1);
  poll();
  assert(remote(pcb_of(5)) == ip4(10, 5, 5, 5));
  // The longest name arrives whole.
  char longest[NAME];
  memset(longest, 'a', 253);
  longest[253] = 0;
  for (int k = 63; k < 253; k += 64) longest[k] = '.';
  start(6, CMD_CONNECT, 0, 80, longest);
  poll();
  assert(!strcmp(asked[asked_count - 1].name, longest));
}

static void an_answer_for_an_earlier_connect_is_ignored(void) {
  fresh();
  start(0, CMD_CONNECT, 0, 443, "first.example");
  poll();
  // The app gives up and connects somewhere else on the same slot.
  start(0, CMD_CLOSE, 0, 0, NULL);
  poll();
  assert(state(0) == IDLE);
  start(0, CMD_CONNECT, 0, 443, "second.example");
  poll();
  assert(asked_count == 2);
  // The first name's answer comes late: it must not become the second's address.
  assert(dns_answer("first.example", 6, 6, 6, 6) == 1);
  poll();
  assert(!pcb_of(0) && state(0) == CONNECTING);
  dns_answer("second.example", 10, 9, 9, 9);
  poll();
  assert(remote(pcb_of(0)) == ip4(10, 9, 9, 9));
  // Likewise a connection's callbacks after its slot moved on.
  struct tcp_pcb *old = pcb_of(0);
  void *old_arg = old->arg;
  start(0, CMD_CONNECT, ip4(1, 2, 3, 4), 80, NULL);
  poll();
  assert(pcb_of(0) != old && (old->closed || old->aborted));
  on_connected(old_arg, old, ERR_OK);
  on_error(old_arg, ERR_RST);
  poll();
  assert(state(0) == CONNECTING && pcb_of(0) && !s[0].connected && !s[0].failed);
}

// Feed the relay's reply a byte at a time, a poll between each.
static void relay_says(int i, const unsigned char *reply, unsigned n) {
  for (unsigned k = 0; k < n; k++) {
    assert(net_deliver(pcb_of(i), reply + k, 1) == ERR_OK);
    poll();
  }
}
static void relay_handshake_in_pieces(void) {
  fresh();
  online = 2;
  poll();
  start(0, CMD_CONNECT, 0, 443, "example.com");
  poll();
  struct tcp_pcb *pcb = pcb_of(0);
  assert(remote(pcb) == ip4(192, 168, 77, 2) && pcb->port == 1081 && asked_count == 0);
  poll();
  assert(pcb->out_len == 0);  // nothing is said before the connection is up
  net_connected(pcb);
  poll();
  assert(pcb->out_len == 3 && !memcmp(pcb->out, "\5\1\2", 3));
  relay_says(0, (const unsigned char *)"\5\2", 2);
  assert(pcb->out_len == 3 + 20 && !memcmp(pcb->out + 3, "\1\6violet\13secret-pass", 20));
  relay_says(0, (const unsigned char *)"\1\0", 2);
  static const unsigned char request[] = "\5\1\0\3\13example.com\1\273";
  assert(pcb->out_len == 23 + 18 && !memcmp(pcb->out + 23, request, 18));
  assert(state(0) == CONNECTING);
  // The reply (an IPv4 bound address) and the server's first bytes in one segment.
  unsigned char reply[15] = {5, 0, 0, 1, 0, 0, 0, 0, 0, 0, 'H', 'E', 'L', 'L', 'O'};
  relay_says(0, reply, 9);
  assert(state(0) == CONNECTING && slot(0)->rx_head == 0);
  assert(net_deliver(pcb, reply + 9, 6) == ERR_OK);
  poll();
  assert(state(0) == OPEN);
  unsigned char buf[32];
  assert(app_read(0, buf, sizeof buf) == 5 && !memcmp(buf, "HELLO", 5));
  // After the handshake the bytes are the app's, both ways.
  app_send(0, "GET /", 5);
  poll();
  assert(pcb->out_len == 41 + 5 && !memcmp(pcb->out + 41, "GET /", 5));
  assert(net_deliver(pcb, "200", 3) == ERR_OK);
  assert(app_read(0, buf, sizeof buf) == 3 && !memcmp(buf, "200", 3));
  poll();
  assert(pcb->rcv_wnd == TCP_WND && live_pbufs == 0);

  // By address, with a domain-name bound address in the reply.
  start(1, CMD_CONNECT, ip4(8, 8, 4, 4), 53, NULL);
  poll();
  pcb = pcb_of(1);
  net_connected(pcb);
  poll();
  relay_says(1, (const unsigned char *)"\5\2\1\0", 4);
  assert(pcb->out_len == 23 + 10 && !memcmp(pcb->out + 23, "\5\1\0\1\10\10\4\4\0\65", 10));
  relay_says(1, (const unsigned char *)"\5\0\0\3\3abc\0\0", 9);
  assert(state(1) == CONNECTING);
  relay_says(1, (const unsigned char *)"\0", 1);
  assert(state(1) == OPEN && slot(1)->rx_head == 0);
}

static void relay_refusals_and_stalls(void) {
  static const struct {
    const char *said;
    unsigned n, error;
  } cases[] = {
      {"\5\377", 2, E_REFUSED},                // no acceptable method
      {"\5\2\1\1", 4, 13},                     // wrong password
      {"\5\2\1\0\5\5\0\1", 8, E_REFUSED},      // connection refused
      {"\5\2\1\0\5\4\0\1", 8, E_UNREACH},      // host unreachable
      {"\5\2\1\0\5\3\0\1", 8, E_NETUNREACH},   // network unreachable
      {"\5\2\1\0\5\6\0\1", 8, E_TIMEOUT},      // TTL expired
      {"\5\2\1\0\5\0\0\7", 8, E_REFUSED},      // an address type that does not exist
      {"\5\2\1\0\4\0\0\1", 8, E_REFUSED},      // not SOCKS5
  };
  for (unsigned c = 0; c < sizeof cases / sizeof *cases; c++) {
    fresh();
    online = 2;
    poll();
    start(0, CMD_CONNECT, 0, 443, "example.com");
    poll();
    net_connected(pcb_of(0));
    poll();
    for (unsigned k = 0; k < cases[c].n && pcb_of(0); k++) {
      net_deliver(pcb_of(0), cases[c].said + k, 1);
      poll();
    }
    assert(state(0) == FAILED && slot(0)->error == cases[c].error && live_tcp == 0 && live_pbufs == 0);
  }
  // A relay that stops answering at each step; one that hangs up; one unknown.
  for (unsigned said = 0; said <= 4; said += 2) {
    fresh();
    online = 2;
    poll();
    start(0, CMD_CONNECT, 0, 443, "example.com");
    start(1, CMD_CONNECT, 0, 443, "example.org");
    poll();
    net_connected(pcb_of(0));
    poll();
    relay_says(0, (const unsigned char *)"\5\2\1\0", said);
    now_ms += RELAY_MS - 1;
    poll();
    assert(state(0) == CONNECTING);
    now_ms++;
    poll();
    assert(state(0) == FAILED && slot(0)->error == E_TIMEOUT);
  }
  fresh();
  online = 2;
  poll();
  start(0, CMD_CONNECT, 0, 443, "example.com");
  poll();
  net_connected(pcb_of(0));
  poll();
  pcb_of(0)->recv(pcb_of(0)->arg, pcb_of(0), NULL, ERR_OK);
  poll();
  assert(state(0) == FAILED && slot(0)->error == E_REFUSED);
  relay_known = 0;
  start(0, CMD_CONNECT, 0, 443, "example.com");
  poll();
  assert(state(0) == FAILED && slot(0)->error == E_NETUNREACH);
  // A handshake message lwIP has no room for is sent on a later poll.
  relay_known = 1;
  start(0, CMD_CONNECT, 0, 443, "example.com");
  poll();
  refuse_writes = 1;
  net_connected(pcb_of(0));
  poll();
  poll();
  assert(pcb_of(0)->out_len == 0 && state(0) == CONNECTING);
  refuse_writes = 0;
  poll();
  assert(pcb_of(0)->out_len == 3);
}

static void received_bytes_hold_no_lwip_buffers(void) {
  fresh();
  open_tcp(0);
  struct tcp_pcb *pcb = pcb_of(0);
  unsigned char data[RX * 3], back[RX * 3];
  for (unsigned k = 0; k < sizeof data; k++) data[k] = k * 31 >> 3;
  // The app reads nothing: the peer may send exactly one ring's worth.
  unsigned sent = 0;
  while (pcb->rcv_wnd) {
    unsigned n = pcb->rcv_wnd < 1460 ? pcb->rcv_wnd : 1460;
    assert(net_deliver(pcb, data + sent, n) == ERR_OK);
    assert(live_pbufs == 0);
    sent += n;
    poll();
  }
  assert(sent == RX && slot(0)->rx_head - slot(0)->rx_tail == RX);
  // A peer that ignores the closed window is refused, and corrupts nothing.
  assert(net_deliver(pcb, data, 100) == ERR_MEM && slot(0)->rx_head == RX);
  // As the app reads, the window reopens by as much, and the bytes arrive in order.
  unsigned got = 0;
  while (got < sizeof data) {
    got += app_read(0, back + got, 1000);
    poll();
    assert(pcb->rcv_wnd <= RX - (slot(0)->rx_head - slot(0)->rx_tail));
    while (pcb->rcv_wnd && sent < sizeof data) {
      unsigned n = pcb->rcv_wnd < 700 ? pcb->rcv_wnd : 700;
      if (n > sizeof data - sent) n = sizeof data - sent;
      assert(net_deliver(pcb, data + sent, n) == ERR_OK);
      sent += n;
    }
    assert(live_pbufs == 0);
  }
  assert(sent == sizeof data && !memcmp(back, data, sizeof data));
  poll();
  assert(pcb->rcv_wnd == TCP_WND);
  // The peer closes: the app sees it after the bytes.
  assert(net_deliver(pcb, "bye", 3) == ERR_OK);
  pcb->recv(pcb->arg, pcb, NULL, ERR_OK);
  poll();
  assert(slot(0)->state == PEER_CLOSED && app_read(0, back, 10) == 3);
  // Bytes that arrive with the connection, before the poll that opens it.
  start(1, CMD_CONNECT, ip4(9, 9, 9, 9), 22, NULL);
  poll();
  net_connected(pcb_of(1));
  assert(net_deliver(pcb_of(1), "SSH-2.0", 7) == ERR_OK);
  assert(slot(1)->rx_head == 0);
  poll();
  assert(state(1) == OPEN && app_read(1, back, 10) == 7 && !memcmp(back, "SSH-2.0", 7));
}

static void stalled_senders_share_the_heap(void) {
  fresh();
  unsigned char data[TX];
  memset(data, 'x', sizeof data);
  // One sender alone may fill its connection's send buffer.
  open_tcp(0);
  app_send(0, data, TX);
  poll();
  assert(pcb_of(0)->out_len == TCP_SND_BUF);
  // Twenty whose peers never acknowledge: together at most half the heap,
  // so the connection that is being acknowledged keeps moving.
  for (int i = 1; i <= 20; i++) open_tcp(i);
  for (int i = 1; i <= 20; i++) app_send(i, data, TX);
  for (int round = 0; round < 50; round++) poll();
  unsigned queued = 0;
  for (int i = 1; i <= 20; i++) {
    assert(pcb_of(i)->out_len >= MIN_SHARE && pcb_of(i)->out_len <= MEM_SIZE / 2 / 20 + MIN_SHARE);
    queued += pcb_of(i)->out_len;
  }
  assert(queued <= MEM_SIZE / 2 + 20 * MIN_SHARE);
  open_tcp(21);
  for (int round = 0; round < 40; round++) {
    app_send(21, data, 100);
    poll();
    net_ack(pcb_of(21));
  }
  assert(pcb_of(21)->out_len == 4000);
  // Closing a connection with unacknowledged bytes resets it; an idle one closes.
  struct tcp_pcb *stuck = pcb_of(1), *idle = pcb_of(21);
  start(1, CMD_CLOSE, 0, 0, NULL);
  start(21, CMD_CLOSE, 0, 0, NULL);
  poll();
  assert(stuck->aborted && idle->closed && !idle->aborted && state(1) == IDLE);
  // Shutting down sending waits for the ring to drain.
  net_ack(pcb_of(2));
  slot(2)->shut = 1;
  for (int round = 0; round < 200 && !s[2].shut; round++) {
    poll();
    net_ack(pcb_of(2));
  }
  assert(s[2].shut && slot(2)->shut == 1 && pcb_of(2)->shut && pcb_of(2)->out_len == TX);
}

static void frame(int i, unsigned ip, unsigned port, const char *name, const char *payload) {
  unsigned char f[600];
  unsigned nl = name ? strlen(name) : 0, pl = strlen(payload), body = 7 + nl + pl;
  f[0] = body; f[1] = body >> 8;
  f[2] = ip; f[3] = ip >> 8; f[4] = ip >> 16; f[5] = ip >> 24;
  f[6] = port; f[7] = port >> 8; f[8] = nl;
  memcpy(f + 9, name, nl);
  memcpy(f + 9 + nl, payload, pl);
  app_send(i, f, body + 2);
}
static void udp_names_resolve_without_waiting(void) {
  fresh();
  struct netif netif;
  IP4_ADDR(&netif.ip_addr, 192, 168, 1, 50);
  netif_default = &netif;
  start(0, CMD_UDP, 0, 0, NULL);
  start(1, CMD_UDP, 0, 0, NULL);
  poll();
  poll();
  assert(state(0) == OPEN && slot(0)->local_port == 40000 && slot(1)->local_port == 40001);
  assert(slot(0)->local_ip == ip4(192, 168, 1, 50));
  frame(0, ip4(8, 8, 8, 8), 123, NULL, "literal");
  poll();
  assert(sent_udp.count == 1 && __builtin_bswap32(sent_udp.to.addr) == ip4(8, 8, 8, 8) && sent_udp.port == 123);
  assert(sent_udp.len == 7 && !memcmp(sent_udp.data, "literal", 7));
  // A datagram to a name waits for its answer; the one behind it waits too,
  // and the other socket is not held up.
  frame(0, 0, 5000, "slow.example", "first");
  frame(0, ip4(8, 8, 8, 8), 123, NULL, "second");
  poll();
  assert(sent_udp.count == 1 && asked_count == 1);
  for (int round = 0; round < 20; round++) {
    now_ms += 100;
    frame(1, ip4(1, 1, 1, 1), 53, NULL, "other");
    poll();
    assert(sent_udp.count == 2u + round && !memcmp(sent_udp.data, "other", 5));
  }
  assert(asked_count == 1);  // asked once, not every poll
  dns_answer("slow.example", 10, 7, 7, 7);
  poll();
  assert(sent_udp.count == 23 && !memcmp(sent_udp.data, "second", 6) && slot(0)->tx_tail == slot(0)->tx_head);
  // A name with no answer: its datagram is dropped after the deadline, the
  // next goes out, and the late answer is not taken for the next name's.
  frame(0, 0, 5000, "silent.example", "lost");
  frame(0, 0, 5001, "later.example", "kept");
  poll();
  now_ms += RESOLVE_MS;
  poll();
  assert(sent_udp.count == 23 && asked_count == 3 && !strcmp(asked[2].name, "later.example"));
  dns_answer("silent.example", 6, 6, 6, 6);
  poll();
  assert(sent_udp.count == 23);
  dns_answer("later.example", 10, 8, 8, 8);
  poll();
  assert(sent_udp.count == 24 && __builtin_bswap32(sent_udp.to.addr) == ip4(10, 8, 8, 8) && sent_udp.port == 5001);
  assert(!memcmp(sent_udp.data, "kept", 4));
  // A failed lookup and a name that cannot be asked drop only their datagram.
  frame(0, 0, 1, "bad.example", "x");
  frame(0, 0, 2, "now.example", "quick");
  poll();
  assert(sent_udp.count == 25 && sent_udp.port == 2);
  // Datagrams in: framed into the ring; dropped when it is full.
  ip_addr_t from;
  IP4_ADDR(&from, 4, 3, 2, 1);
  unsigned in = 0;
  for (int k = 0; k < 40; k++) {
    struct pbuf *p = pbuf_alloc(PBUF_TRANSPORT, 1000, PBUF_RAM);
    memset(p->payload, 'a' + k % 26, 1000);
    s[1].udp->recv(s[1].udp->arg, s[1].udp, p, &from, 4242);
    if (slot(1)->rx_head != in) in = slot(1)->rx_head;
  }
  assert(in == 16 * 1009 && live_pbufs == 0);
  unsigned char got[1009];
  assert(app_read(1, got, 1009) == 1009);
  assert(got[0] == (1007 & 255) && got[1] == 1007 >> 8 && got[2] == 1 && got[5] == 4 && got[6] == (4242 & 255) && got[9] == 'a');
  // Garbage in the send ring is dropped, not followed.
  app_send(1, "\1\0zz", 4);
  poll();
  assert(slot(1)->tx_tail == slot(1)->tx_head);
  // Through the phone's relay there is no UDP.
  online = 2;
  poll();
  start(2, CMD_UDP, 0, 0, NULL);
  poll();
  assert(state(2) == FAILED && slot(2)->error == E_NETUNREACH);
}

static void offline_and_restart(void) {
  fresh();
  open_tcp(0);
  start(1, CMD_CONNECT, 0, 80, "slow.example");
  start(2, CMD_UDP, 0, 0, NULL);
  poll();
  online = 0;
  poll();
  for (int i = 0; i < 3; i++) assert(state(i) == FAILED && slot(i)->error == E_UNREACH);
  assert(state(3) == IDLE && live_tcp == 0 && live_udp == 0 && ((unsigned *)region)[1] == 0);
  start(3, CMD_CONNECT, ip4(1, 1, 1, 1), 80, NULL);
  poll();
  assert(state(3) == FAILED && slot(3)->error == E_UNREACH);
  // The answer for a name asked before the link dropped changes nothing.
  dns_answer("slow.example", 10, 0, 0, 9);
  online = 1;
  poll();
  assert(state(1) == FAILED && live_tcp == 0);
  // A Radio that restarted finds connections it does not have: they are reset.
  open_tcp(4);
  start(5, CMD_UDP, 0, 0, NULL);
  poll();
  memset(s, 0, sizeof s);
  live_tcp = live_udp = 0;
  poll();
  assert(state(4) == FAILED && slot(4)->error == E_RESET && state(5) == FAILED && state(3) == FAILED && state(6) == IDLE);
  // A page that is not this layout is started afresh.
  ((unsigned *)region)[0] = 0x3154454e;  // "NET1"
  poll();
  assert(((unsigned *)region)[0] == 0x3254454e && ((unsigned *)region)[2] == SLOTS && state(4) == IDLE);
}


// The Open Runtime frees a slot the moment an app closes it and hands the
// lowest free slot to the next connect, so a slot is often reused before
// Radio has seen the close. Whatever the old connection still receives in
// between must not reach the new one.
static void a_reused_slot_starts_empty(void) {
  fresh();
  unsigned char buf[64];
  open_tcp(0);
  struct tcp_pcb *old = pcb_of(0);
  app_send(0, "0123456789", 10);
  poll();
  assert(net_deliver(old, "unread", 6) == ERR_OK);
  assert(slot(0)->tx_tail == 10 && slot(0)->rx_head == 6);
  // App A closes; app B's connect gets the slot; neither has reached Radio.
  start(0, CMD_CLOSE, 0, 0, NULL);
  start(0, CMD_CONNECT, ip4(10, 0, 0, 7), 443, NULL);
  // A's server is still sending: lwIP hands the bytes to the old connection.
  assert(net_deliver(old, "A's secret bytes", 16) == ERR_OK);
  // B cannot read them, before Radio's poll or after.
  assert(app_read(0, buf, sizeof buf) == 0);
  // B queues its first bytes before the poll; a pump that looked at the slot
  // before the command arrived must not send them down A's connection.
  app_send(0, "B's request", 11);
  unsigned sent_before = old->out_len;
  pump(0, now_ms);
  assert(old->out_len == sent_before && pcb_of(0) == old);
  poll();
  assert(old->closed || old->aborted);
  assert(slot(0)->rx_head == 0 && slot(0)->rx_tail == 0 && slot(0)->tx_tail == 0);
  assert(app_read(0, buf, sizeof buf) == 0 && state(0) == CONNECTING);
  net_connected(pcb_of(0));
  poll();
  assert(state(0) == OPEN && slot(0)->rx_head == slot(0)->rx_tail);
  assert(app_read(0, buf, sizeof buf) == 0);
  poll();
  assert(pcb_of(0)->out_len == 11 && !memcmp(pcb_of(0)->out, "B's request", 11));
  assert(net_deliver(pcb_of(0), "B's reply", 9) == ERR_OK);
  assert(app_read(0, buf, sizeof buf) == 9 && !memcmp(buf, "B's reply", 9));
  // The old connection's callbacks, later still, change nothing.
  on_received(tag(0) - 256, old, pbuf_alloc(PBUF_TRANSPORT, 5, PBUF_RAM), ERR_OK);
  assert(slot(0)->rx_head == 9 && live_pbufs == 0);

  // The same for datagrams: a UDP socket closed and its slot reused.
  ip_addr_t from;
  IP4_ADDR(&from, 6, 6, 6, 6);
  start(1, CMD_UDP, 0, 0, NULL);
  poll();
  struct udp_pcb *udp = s[1].udp;
  void *udp_arg = udp->arg;
  assert(slot(1)->local_port != 0);
  start(1, CMD_CLOSE, 0, 0, NULL);
  start(1, CMD_CONNECT, ip4(10, 0, 0, 8), 80, NULL);
  struct pbuf *p = pbuf_alloc(PBUF_TRANSPORT, 100, PBUF_RAM);
  memset(p->payload, 'A', 100);
  udp->recv(udp_arg, udp, p, &from, 9);
  assert(app_read(1, buf, sizeof buf) == 0);
  poll();
  assert(slot(1)->rx_head == 0 && slot(1)->local_port == 0 && slot(1)->local_ip == 0);
  net_connected(pcb_of(1));
  poll();
  assert(state(1) == OPEN && app_read(1, buf, sizeof buf) == 0 && live_pbufs == 0);

  // Offline, a command is refused, and the ring is emptied all the same.
  open_tcp(2);
  assert(net_deliver(pcb_of(2), "left over", 9) == ERR_OK);
  online = 0;
  start(2, CMD_CLOSE, 0, 0, NULL);
  start(2, CMD_CONNECT, ip4(10, 0, 0, 9), 80, NULL);
  poll();
  assert(state(2) == FAILED && slot(2)->rx_head == 0 && app_read(2, buf, sizeof buf) == 0);
}

// A "name" that a resolver reads as an address is never resolved or dialled:
// the Shield would have judged the name, and the connection gone to 10.0.0.9.
static void a_name_that_is_an_address_is_refused(void) {
  static const char *const forms[] = {"10.0.0.9", "167772169", "0x0a000009", "0X0A000009", "10.9", "10.0.9", "012.0.0.011", "0", "1.2.3.4"};
  for (unsigned mode = 1; mode <= 2; mode++) {
    fresh();
    online = mode;
    poll();
    for (unsigned k = 0; k < sizeof forms / sizeof *forms; k++) {
      start(k, CMD_CONNECT, 0, 443, forms[k]);
      poll();
      assert(state(k) == FAILED && slot(k)->error == E_NONAME && !pcb_of(k));
    }
    assert(asked_count == 0 && made_count == 0 && live_tcp == 0);
  }
  fresh();
  start(0, CMD_UDP, 0, 0, NULL);
  poll();
  for (unsigned k = 0; k < sizeof forms / sizeof *forms; k++) frame(0, 0, 53, forms[k], "to an address");
  frame(0, 0, 53, "now.example", "to a name");
  poll();
  assert(asked_count == 0 && sent_udp.count == 1 && !memcmp(sent_udp.data, "to a name", 9));
  assert(slot(0)->tx_tail == slot(0)->tx_head);
  // Names that merely have digits in them are names.
  start(1, CMD_CONNECT, 0, 80, "1.example");
  start(2, CMD_CONNECT, 0, 80, "0x10.net");
  poll();
  assert(asked_count == 2 && state(1) == CONNECTING && state(2) == CONNECTING);
}

// An app connects and closes in a loop against a server that completes the
// handshake, acknowledges the FIN and never closes: every closed connection
// stays in lwIP. They must not use up lwIP's connections.
static void closing_connections_give_way_to_new_ones(void) {
  fresh();
  peers_never_close = 1;
  for (int round = 0; round < 100; round++) {
    start(0, CMD_CONNECT, ip4(203, 0, 113, 5), 80, NULL);
    poll();
    assert(pcb_of(0));
    net_connected(pcb_of(0));
    poll();
    assert(state(0) == OPEN);
    start(0, CMD_CLOSE, 0, 0, NULL);
    poll();
  }
  assert(lingering > 0 && live_tcp + lingering <= MEMP_NUM_TCP_PCB);
  // Every slot can still connect, at once.
  for (int i = 0; i < SLOTS; i++) open_tcp(i);
  assert(live_tcp == SLOTS && live_tcp + lingering <= MEMP_NUM_TCP_PCB);
  // And a connection that is in use is never the one given up.
  for (unsigned k = 0; k < made_count; k++) assert(!made[k]->aborted || made[k]->closed);
}

// lwIP cannot forget a question, so an app that asks for names nobody
// answers, closes and asks for more would fill lwIP's table and leave every
// other app's (and the Sentinel's) names unasked. Questions are counted by
// owner, whatever became of the slots that asked them.
static unsigned own_questions(unsigned owner) {
  unsigned n = 0;
  for (int k = 0; k < ASKS; k++) n += asks[k].used && asks[k].owner == owner;
  return n;
}
static void one_app_cannot_fill_the_name_table(void) {
  fresh();
  char name[64];
  unsigned serial = 0;
  // The hog: 15 slots, a fresh unanswered name on each, closed and asked
  // again every poll, far faster than lwIP gives a question up.
  for (int round = 0; round < 60; round++) {
    as_owner = 7;
    for (int i = 0; i < 15; i++) {
      snprintf(name, sizeof name, "h%u.hog.example", serial++);
      start(i, CMD_CONNECT, 0, 80, name);
    }
    poll();
    now_ms += 20;
    assert(own_questions(7) == ASKS_EACH && dns_out() == ASKS_EACH);
  }
  assert(asked_count == ASKS_EACH && table_full == 0);
  // Another app's name, and the Open Runtime's own, are asked at once.
  as_owner = 8;
  start(20, CMD_CONNECT, 0, 443, "peer.example");
  as_owner = 0;
  start(31, CMD_CONNECT, 0, 7001, "sentinel.example");
  poll();
  assert(asked_count == ASKS_EACH + 2 && !strcmp(asked[4].name, "peer.example") && !strcmp(asked[5].name, "sentinel.example"));
  assert(dns_answer("peer.example", 10, 3, 3, 3) == 1 && dns_answer("sentinel.example", 10, 4, 4, 4) == 1);
  poll();
  assert(remote(pcb_of(20)) == ip4(10, 3, 3, 3) && remote(pcb_of(31)) == ip4(10, 4, 4, 4));
  assert(own_questions(8) == 0 && own_questions(0) == 0);
  // A UDP socket of the hog's is held to the same count, and loses only its own datagrams.
  as_owner = 7;
  start(15, CMD_UDP, 0, 0, NULL);
  poll();
  frame(15, 0, 53, "udp.hog.example", "x");
  poll();
  assert(asked_count == ASKS_EACH + 2 && own_questions(7) == ASKS_EACH);
  // A waiting connect of the hog's fails at its deadline like any unresolved name.
  now_ms += RESOLVE_MS;
  poll();
  for (int i = 0; i < 15; i++) assert(state(i) == FAILED && slot(i)->error == E_NONAME);
  assert(own_questions(7) == ASKS_EACH);
  // When lwIP gives one of its questions up, the hog may ask one more: the
  // datagram's name, or a connect's, whichever its slots ask first.
  assert(dns_answer(asked[0].name, -1, 0, 0, 0) == 1);
  assert(own_questions(7) == ASKS_EACH - 1);
  start(0, CMD_CONNECT, 0, 80, "again.hog.example");
  poll();
  assert(own_questions(7) == ASKS_EACH && asked_count == ASKS_EACH + 3 && !strcmp(asked[6].name, "again.hog.example"));
  // An answer for a question whose slot has moved on is counted off and goes nowhere.
  start(0, CMD_CLOSE, 0, 0, NULL);
  poll();
  assert(own_questions(7) == ASKS_EACH);
  assert(dns_answer("again.hog.example", 6, 6, 6, 6) == 1);
  poll();
  assert(own_questions(7) == ASKS_EACH - 1 && !pcb_of(0) && state(0) == IDLE);

  // Many apps at once: together they leave the Open Runtime its questions
  // and lwIP's table room for Radio's own.
  fresh();
  for (int i = 0; i < 29; i++) {
    as_owner = 100 + i / 3;  // ten apps, three sockets each (the last has two)
    snprintf(name, sizeof name, "h%d.many.example", i);
    start(i, CMD_CONNECT, 0, 80, name);
  }
  as_owner = 0;
  for (int i = 29; i < SLOTS; i++) {
    snprintf(name, sizeof name, "h%d.system.example", i);
    start(i, CMD_CONNECT, 0, 80, name);
  }
  for (int round = 0; round < 50; round++) {
    // Each app closes and asks again, every poll.
    for (int i = 0; i < 29; i++) {
      as_owner = 100 + i / 3;
      snprintf(name, sizeof name, "h%d-%d.many.example", i, round);
      start(i, CMD_CONNECT, 0, 80, name);
    }
    poll();
    unsigned apps = 0;
    for (int k = 0; k < ASKS; k++) apps += asks[k].used && asks[k].owner != 0;
    assert(apps <= APP_ASKS && own_questions(0) == 3 && dns_out() <= ASKS && dns_out() + 4 <= DNS_TABLE_SIZE);
    for (unsigned o = 100; o < 110; o++) assert(own_questions(o) <= ASKS_EACH);
  }
  assert(table_full == 0);
}

// Radio serves only the slots whose rings the supervisor mapped. A supervisor
// from before the 32-slot layout maps 49 pages and says nothing: seven slots,
// and nothing touched past the 49th page.
static void an_old_supervisor_gets_the_slots_it_mapped(void) {
  static const struct {
    unsigned extent, slots;
  } cases[] = {{0, 7}, {0x31000, 7}, {0x35000, 8}, {0x3b000, 9}, {0x40000, 9}, {0x49000, 10}, {0xcd000, 32}, {0xd0000, 32},
               {0x31001, 7}, {0x30000, 7}, {0xfffff000, 7}, {0x200000, 7}};
  for (unsigned c = 0; c < sizeof cases / sizeof *cases; c++) {
    extent = cases[c].extent;
    fresh();
    extent = MAPPED;
    unsigned n = ((unsigned *)region)[2];
    assert(n == cases[c].slots && ((unsigned *)region)[0] == MAGIC && ((unsigned *)region)[5] == cases[c].extent);
    unsigned mapped = cases[c].slots == 7 ? 0x31000 : cases[c].extent;
    // Everything past what is mapped (and the Bluetooth pages) must stay untouched.
    memset(region + mapped, 0x77, REGION - mapped);
    if (mapped > HOLE) memset(region + HOLE, 0x77, HOLE_END - HOLE);
    unsigned char data[RX];
    memset(data, 'd', sizeof data);
    for (unsigned i = 0; i < n; i++) {
      open_tcp(i);
      app_send(i, data, TX);
      poll();
      for (unsigned done = 0; done < RX; done += 4096) assert(net_deliver(pcb_of(i), data, 4096) == ERR_OK);
    }
    // A command on a slot that is not served is never taken, and nothing of it is touched.
    if (n < SLOTS) {
      volatile struct slot *q = slot(n);
      unsigned seq = q->cmd_seq + 1;
      q->cmd = CMD_CONNECT;
      q->ip = ip4(10, 0, 0, 1);
      q->port = 80;
      q->cmd_seq = seq;
      unsigned made_before = made_count;
      poll();
      poll();
      assert(q->done_seq != seq && made_count == made_before && !pcb_of(n));
    }
    online = 0;
    poll();
    online = 1;
    poll();
    for (unsigned k = mapped; k < REGION; k++) assert(region[k] == 0x77 || (k >= HOLE_END && k < mapped));
    if (mapped > HOLE)
      for (unsigned k = HOLE; k < HOLE_END; k++) assert(region[k] == 0x77);
  }
  // The count is part of the layout: a restarted Radio that finds another count starts the page afresh.
  fresh();
  ((unsigned *)region)[2] = 7;
  poll();
  assert(((unsigned *)region)[2] == SLOTS && ((unsigned *)region)[5] == MAPPED);
}

int main(void) {
  RUN(layout_is_net2_and_keeps_out_of_the_hole);
  RUN(a_connect_that_never_answers_delays_nobody);
  RUN(names_resolve_without_waiting);
  RUN(an_answer_for_an_earlier_connect_is_ignored);
  RUN(relay_handshake_in_pieces);
  RUN(relay_refusals_and_stalls);
  RUN(received_bytes_hold_no_lwip_buffers);
  RUN(stalled_senders_share_the_heap);
  RUN(udp_names_resolve_without_waiting);
  RUN(offline_and_restart);
  RUN(a_reused_slot_starts_empty);
  RUN(a_name_that_is_an_address_is_refused);
  RUN(closing_connections_give_way_to_new_ones);
  RUN(one_app_cannot_fill_the_name_table);
  RUN(an_old_supervisor_gets_the_slots_it_mapped);
  puts("sockets.c: all tests passed");
  return 0;
}
