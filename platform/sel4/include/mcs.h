#pragma once
#include <sel4/sel4.h>
/* The same code builds for seL4 with and without MCS (KernelIsMCS). Under
   MCS a receive names a Reply object and a reply is a Send on it; every
   process keeps its Reply object in REPLY_SLOT, where the supervisor puts it.
   Without MCS the kernel keeps the caller in the thread and REPLY_SLOT is
   unused. */
enum { REPLY_SLOT = 31 };
#ifdef CONFIG_KERNEL_MCS
#define IPC_RECV(ep, badge, reply) seL4_Recv((ep), (badge), (reply))
#define IPC_NBRECV(ep, badge, reply) seL4_NBRecv((ep), (badge), (reply))
#define IPC_REPLY(reply, info) seL4_Send((reply), (info))
#else
#define IPC_RECV(ep, badge, reply) seL4_Recv((ep), (badge))
#define IPC_NBRECV(ep, badge, reply) seL4_NBRecv((ep), (badge))
#define IPC_REPLY(reply, info) seL4_Reply(info)
#endif
