#pragma once
#define NO_SYS 1
#define MEM_ALIGNMENT 4
#define MEM_SIZE 24576
#define LWIP_SOCKET 0
#define LWIP_NETCONN 0
#define LWIP_STATS 0
#define LWIP_DHCP 1
#define LWIP_DNS 1
#define LWIP_DNS_SECURE 7
#define LWIP_IPV6 0
#define LWIP_IPV4 1
#define LWIP_IGMP 0
#define IP_FORWARD 0
#define IP_REASSEMBLY 0
#define IP_FRAG 0
#define LWIP_RAW 0
#define TCP_MSS 1460
#define TCP_WND (4*TCP_MSS)
#define TCP_SND_BUF (4*TCP_MSS)
#define TCP_SND_QUEUELEN 16
// Open apps' sockets (32 slots, sockets.c), Radio's own HTTPS connection, and
// room for connections that are closing; sockets.c lets lwIP take a closing
// one back when a connect finds none free, and the connections parked on
// listening slots until accepted (sockets.c PARKED; tools/test_net_layout.py
// checks these against the slot count).
#define MEMP_NUM_TCP_PCB 48
// Listening slots: an app's listener takes one of its slots, so no more than
// the apps' slots, and no fewer than one each for the busiest two apps.
#define MEMP_NUM_TCP_PCB_LISTEN 8
// lwIP counts connections that have not been accepted (SYN received, or
// parked by sockets.c) against the listener's backlog and drops SYNs past it.
#define TCP_LISTEN_BACKLOG 1
// A listener may take a port its last connections still hold in TIME_WAIT.
#define SO_REUSE 1
// Open apps' UDP sockets (one a slot), DNS (one for each source port it may
// use at once, DNS_MAX_SOURCE_PORTS) and DHCP, and one to spare.
#define MEMP_NUM_UDP_PCB 38
// Segments queued to send: at least one for every slot that is sending.
#define MEMP_NUM_TCP_SEG 64
// Names lwIP may be asking for at once. A question cannot be cancelled, so
// sockets.c counts them by app (3 each, 36 for all twelve apps, 3 for the Open
// Runtime's own sockets); the last 4 are Radio's own. About 280 bytes each.
#define DNS_TABLE_SIZE 44
// Source ports those questions are spread over (each is a UDP PCB).
#define DNS_MAX_SOURCE_PORTS 4
#define PBUF_POOL_SIZE 16
// The local ports lwIP picks for connections and unbound UDP sockets: the
// lower half of IANA's dynamic range. The Open Runtime picks ports for apps'
// port 0 binds and loopback from the upper half (loopback.rs EPHEMERAL), so a
// port it gives a listener is never one lwIP already handed out.
#define TCP_LOCAL_PORT_RANGE_START 0xc000
#define TCP_LOCAL_PORT_RANGE_END 0xdfff
#define TCP_ENSURE_LOCAL_PORT_RANGE(port) ((u16_t)(((port) & 0x1fff) + 0xc000))
#define UDP_LOCAL_PORT_RANGE_START 0xc000
#define UDP_LOCAL_PORT_RANGE_END 0xdfff
#define UDP_ENSURE_LOCAL_PORT_RANGE(port) ((u16_t)(((port) & 0x1fff) + 0xc000))
#define LWIP_NETIF_HOSTNAME 0
#define LWIP_NETIF_STATUS_CALLBACK 0
#define LWIP_TIMEVAL_PRIVATE 0
#define SYS_LIGHTWEIGHT_PROT 0

#define LWIP_DONT_PROVIDE_BYTEORDER_FUNCTIONS 1
