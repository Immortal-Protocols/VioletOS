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
// one back when a connect finds none free (tools/test_net_layout.py checks
// these against the slot count).
#define MEMP_NUM_TCP_PCB 40
// Open apps' UDP sockets (one a slot), DNS (one for each source port it may
// use at once, DNS_MAX_SOURCE_PORTS) and DHCP, and one to spare.
#define MEMP_NUM_UDP_PCB 38
// Segments queued to send: at least one for every slot that is sending.
#define MEMP_NUM_TCP_SEG 64
// Names lwIP may be asking for at once. A question cannot be cancelled, so
// sockets.c counts them by app (4 each, 32 for all apps, 4 for the Open
// Runtime's own sockets); the last 4 are Radio's own. About 280 bytes each.
#define DNS_TABLE_SIZE 40
// Source ports those questions are spread over (each is a UDP PCB).
#define DNS_MAX_SOURCE_PORTS 4
#define PBUF_POOL_SIZE 16
#define LWIP_NETIF_HOSTNAME 0
#define LWIP_NETIF_STATUS_CALLBACK 0
#define LWIP_TIMEVAL_PRIVATE 0
#define SYS_LIGHTWEIGHT_PROT 0

#define LWIP_DONT_PROVIDE_BYTEORDER_FUNCTIONS 1
