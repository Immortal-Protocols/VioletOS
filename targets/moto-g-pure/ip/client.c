// One IPv4 station and one HTTPS socket. Every IP packet goes over Moto AXI.
#include "lwip/dhcp.h"
#include "lwip/dns.h"
#include "lwip/etharp.h"
#include "lwip/init.h"
#include "lwip/netif.h"
#include "lwip/tcp.h"
#include "lwip/timeouts.h"
#include "netif/ethernet.h"
#include <string.h>
extern int radio_l2_send(const unsigned char *, size_t),
    radio_l2_recv(unsigned char *, size_t);
extern int radio_random(unsigned char *, size_t), guest_https(unsigned char *);
extern unsigned radio_monotonic_ms(void);
static struct netif iface;
static struct tcp_pcb *connection;
static struct pbuf *pending;
static unsigned request_start;
static int established, failed, eof, initialized, attached, dns_state;
static ip_addr_t destination;
static unsigned char frame[1518];
u32_t sys_now(void) { return radio_monotonic_ms(); }
unsigned radio_ip_random(void) {
  unsigned v = 0;
  if (radio_random((unsigned char *)&v, sizeof(v)))
    failed = 1;
  return v;
}
int radio_entropy(unsigned char seed[32]) {
  return radio_random(seed, 32) == 0;
}
static err_t send_frame(struct netif *n, struct pbuf *packet) {
  if (packet->tot_len > sizeof(frame))
    return ERR_BUF;
  u16_t len = pbuf_copy_partial(packet, frame, packet->tot_len, 0);
  return len == packet->tot_len && radio_l2_send(frame, len) == 0 ? ERR_OK
                                                                  : ERR_IF;
}
static err_t setup(struct netif *n) {
  n->name[0] = 'w';
  n->name[1] = 'l';
  n->mtu = 1500;
  n->hwaddr_len = 6;
  n->flags = NETIF_FLAG_BROADCAST | NETIF_FLAG_ETHARP;
  n->output = etharp_output;
  n->linkoutput = send_frame;
  return ERR_OK;
}
void station_net_poll(void) {
  if (!attached)
    return;
  for (unsigned i = 0; i < 8; i++) {
    int len = radio_l2_recv(frame, sizeof(frame));
    if (!len)
      break;
    if (len < 0) {
      failed = 1;
      break;
    }
    struct pbuf *p = pbuf_alloc(PBUF_RAW, (u16_t)len, PBUF_POOL);
    if (p && (pbuf_take(p, frame, (u16_t)len) != ERR_OK ||
              ethernet_input(p, &iface) != ERR_OK))
      pbuf_free(p);
  }
  sys_check_timeouts();
}
static void abort_connection(void) {
  if (connection) {
    tcp_recv(connection, NULL);
    tcp_err(connection, NULL);
    tcp_abort(connection);
    connection = NULL;
  }
  if (pending) {
    pbuf_free(pending);
    pending = NULL;
  }
  established = 0;
}
void station_net_close(void) {
  abort_connection();
  if (attached) {
    dhcp_stop(&iface);
    dhcp_cleanup(&iface);
    netif_set_link_down(&iface);
    netif_set_down(&iface);
    netif_remove(&iface);
    attached = 0;
  }
}
int station_net_join(const unsigned char *mac) {
  station_net_close();
  failed = 0;
  if (!initialized) {
    lwip_init();
    initialized = 1;
  }
  memset(&iface, 0, sizeof(iface));
  if (!netif_add(&iface, NULL, NULL, NULL, NULL, setup, ethernet_input))
    return 0;
  attached = 1;
  memcpy(iface.hwaddr, mac, 6);
  netif_set_default(&iface);
  netif_set_up(&iface);
  netif_set_link_up(&iface);
  unsigned start = sys_now();
  if (dhcp_start(&iface) == ERR_OK) {
    while (!failed && sys_now() - start < 15000) {
      station_net_poll();
      if (!failed && dhcp_supplied_address(&iface))
        return 1;
    }
  }
  station_net_close();
  return 0;
}
static err_t connected(void *arg, struct tcp_pcb *pcb, err_t err) {
  established = err == ERR_OK;
  failed = !established;
  return ERR_OK;
}
static void error(void *arg, err_t err) {
  failed = 1;
  connection = NULL;
}
static err_t received(void *arg, struct tcp_pcb *pcb, struct pbuf *p,
                      err_t err) {
  if (!p) {
    eof = 1;
    return ERR_OK;
  }
  if (pending)
    pbuf_cat(pending, p);
  else
    pending = p;
  return ERR_OK;
}
static void resolved(const char *name, const ip_addr_t *addr, void *arg) {
  if (arg != (void *)(uintptr_t)dns_state)
    return;
  if (addr)
    destination = *addr;
  else
    failed = 1;
}
int tunnel_read(void *ctx, unsigned char *out, size_t size) {
  unsigned start = sys_now();
  while (!pending && !failed && !eof && sys_now() - request_start < 15000 &&
         sys_now() - start < 5000)
    station_net_poll();
  if (!connection || !pending || failed)
    return -1;
  unsigned n = pending->tot_len;
  if (n > size)
    n = size;
  n = pbuf_copy_partial(pending, out, n, 0);
  pending = pbuf_free_header(pending, n);
  tcp_recved(connection, n);
  return n ? (int)n : -1;
}
int tunnel_write(void *ctx, const unsigned char *buf, size_t len) {
  unsigned start = sys_now();
  while (connection && !failed && sys_now() - request_start < 15000 &&
         sys_now() - start < 5000) {
    unsigned n = tcp_sndbuf(connection);
    if (n > len)
      n = len;
    if (n && tcp_write(connection, buf, n, TCP_WRITE_FLAG_COPY) == ERR_OK) {
      tcp_output(connection);
      return n;
    }
    station_net_poll();
  }
  return -1;
}
int station_net_uuid(unsigned char *out) {
  if (!attached || !dhcp_supplied_address(&iface))
    return 0;
  abort_connection();
  failed = 0;
  eof = 0;
  request_start = sys_now();
  static unsigned generation;
  dns_state = ++generation;
  ip_addr_set_zero(&destination);
  err_t rc = dns_gethostbyname("www.uuidgenerator.net", &destination, resolved,
                               (void *)(uintptr_t)dns_state);
  if (rc != ERR_OK && rc != ERR_INPROGRESS)
    return 0;
  unsigned start = sys_now();
  while (ip_addr_isany_val(destination) && !failed && sys_now() - start < 5000)
    station_net_poll();
  if (failed || ip_addr_isany_val(destination))
    return 0;
  connection = tcp_new();
  if (!connection)
    return 0;
  tcp_err(connection, error);
  tcp_recv(connection, received);
  if (tcp_connect(connection, &destination, 443, connected) == ERR_OK) {
    start = sys_now();
    while (!established && !failed && sys_now() - start < 8000)
      station_net_poll();
  }
  int ok = established && !failed && guest_https(out);
  abort_connection();
  return ok;
}
