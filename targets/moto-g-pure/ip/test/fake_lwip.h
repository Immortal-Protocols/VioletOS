// The part of lwIP's raw API that ../sockets.c uses, for its host test
// (test_sockets.c implements it). The sizes come from the real lwipopts.h.
#pragma once
#include <stddef.h>
#include <stdint.h>
#include "../lwipopts.h"

typedef int8_t err_t;
typedef uint16_t u16_t;
enum { ERR_OK = 0, ERR_MEM = -1, ERR_RTE = -4, ERR_INPROGRESS = -5, ERR_VAL = -6, ERR_USE = -8, ERR_ABRT = -13, ERR_RST = -14, ERR_ARG = -16 };

// IPv4 addresses in network byte order, as lwIP keeps them.
typedef struct { uint32_t addr; } ip4_addr_t;
typedef ip4_addr_t ip_addr_t;
#define ip_2_ip4(a) (a)
#define ip4_addr_get_u32(a) ((a)->addr)
#define IP4_ADDR(a, b, c, d, e) ((a)->addr = __builtin_bswap32((uint32_t)(b) << 24 | (uint32_t)(c) << 16 | (uint32_t)(d) << 8 | (uint32_t)(e)))
extern const ip_addr_t fake_any;
// Whether a string is an address written out, as lwIP's ip4addr_aton reads
// one (the test uses this machine's inet_aton, which reads the same forms).
int ipaddr_aton(const char *cp, ip_addr_t *addr);
#define IP4_ADDR_ANY (&fake_any)

struct pbuf {
  struct pbuf *next;
  void *payload;
  u16_t tot_len, len;
};
enum { PBUF_TRANSPORT, PBUF_RAM };
struct pbuf *pbuf_alloc(int layer, u16_t length, int type);
unsigned char pbuf_free(struct pbuf *p);

struct tcp_pcb;
typedef err_t (*tcp_connected_fn)(void *arg, struct tcp_pcb *pcb, err_t err);
typedef err_t (*tcp_recv_fn)(void *arg, struct tcp_pcb *pcb, struct pbuf *p, err_t err);
typedef void (*tcp_err_fn)(void *arg, err_t err);
typedef err_t (*tcp_accept_fn)(void *arg, struct tcp_pcb *newpcb, err_t err);
struct tcp_pcb {
  void *unsent, *unacked;
  ip_addr_t remote_ip;
  u16_t remote_port;
  // The rest is the test's.
  void *arg;
  tcp_recv_fn recv;
  tcp_err_fn err;
  tcp_connected_fn connected;
  ip_addr_t remote;
  unsigned port, snd_buf, rcv_wnd, rcv_ann_wnd, ack_now, out_len, closed, aborted, shut, prio, lingers;
  // A listener: its port and backlog, accept callback; a connection: whether
  // lwIP still counts it against its listener's backlog (delayed).
  tcp_accept_fn accept;
  unsigned listening, local_port, backlog, delayed, reuse;
  unsigned char out[32768];
};
#define TCP_WRITE_FLAG_COPY 1
#define TCP_PRIO_MIN 1
#define TCP_PRIO_NORMAL 64
void tcp_setprio(struct tcp_pcb *pcb, unsigned char prio);
struct tcp_pcb *tcp_new(void);
void tcp_arg(struct tcp_pcb *pcb, void *arg);
void tcp_recv(struct tcp_pcb *pcb, tcp_recv_fn fn);
void tcp_err(struct tcp_pcb *pcb, tcp_err_fn fn);
err_t tcp_connect(struct tcp_pcb *pcb, const ip_addr_t *addr, u16_t port, tcp_connected_fn fn);
err_t tcp_write(struct tcp_pcb *pcb, const void *data, u16_t len, unsigned char flags);
err_t tcp_output(struct tcp_pcb *pcb);
err_t tcp_close(struct tcp_pcb *pcb);
void tcp_abort(struct tcp_pcb *pcb);
err_t tcp_shutdown(struct tcp_pcb *pcb, int rx, int tx);
void tcp_recved(struct tcp_pcb *pcb, u16_t len);
#define tcp_ack_now(pcb) ((pcb)->ack_now = 1)
unsigned tcp_sndbuf(struct tcp_pcb *pcb);
err_t tcp_bind(struct tcp_pcb *pcb, const ip_addr_t *addr, u16_t port);
struct tcp_pcb *tcp_listen_with_backlog_and_err(struct tcp_pcb *pcb, unsigned char backlog, err_t *err);
void tcp_accept(struct tcp_pcb *pcb, tcp_accept_fn fn);
void tcp_backlog_delayed(struct tcp_pcb *pcb);
void tcp_backlog_accepted(struct tcp_pcb *pcb);
#define SOF_REUSEADDR 4
#define ip_set_option(pcb, opt) ((pcb)->reuse |= (opt))

struct udp_pcb {
  u16_t local_port;
  void *arg;
  void (*recv)(void *arg, struct udp_pcb *pcb, struct pbuf *p, const ip_addr_t *addr, u16_t port);
};
typedef void (*udp_recv_fn)(void *arg, struct udp_pcb *pcb, struct pbuf *p, const ip_addr_t *addr, u16_t port);
struct udp_pcb *udp_new(void);
err_t udp_bind(struct udp_pcb *pcb, const ip_addr_t *addr, u16_t port);
void udp_recv(struct udp_pcb *pcb, udp_recv_fn fn, void *arg);
err_t udp_sendto(struct udp_pcb *pcb, struct pbuf *p, const ip_addr_t *addr, u16_t port);
void udp_remove(struct udp_pcb *pcb);

typedef void (*dns_found_callback)(const char *name, const ip_addr_t *addr, void *arg);
err_t dns_gethostbyname(const char *name, ip_addr_t *addr, dns_found_callback found, void *arg);

struct netif { ip_addr_t ip_addr; };
extern struct netif *netif_default;
void sys_check_timeouts(void);
