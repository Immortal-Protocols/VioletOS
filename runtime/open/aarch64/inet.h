// Adapt Linux IPv4 descriptors to the same shared socket rings used by VioletOS
// on ARMv7. Only raw network traffic crosses the QEMU boundary.
extern size_t ring_slots(void);
extern void ring_local(size_t, uint32_t *, uint16_t *);
extern uintptr_t ring_init(uintptr_t, size_t);
extern int ring_connect(uint32_t, uint16_t), ring_udp_open(void);
extern unsigned ring_state(size_t, int *);
extern size_t ring_space(size_t), ring_avail(size_t);
extern size_t ring_send(size_t, const void *, size_t), ring_recv(size_t, void *, size_t, bool);
extern bool ring_udp_send(size_t, uint32_t, uint16_t, const void *, size_t);
extern int ring_udp_recv(size_t, uint32_t *, uint16_t *, void *);
extern void ring_close(size_t), ring_shutdown(size_t);
extern size_t ring_dns(const void *, size_t, void *, size_t);

static unsigned char net_heap[1 << 20] __attribute__((aligned(16)));
void net_panic(void) { say("network runtime panic\n"); halt(); }
// The freestanding ring library targets baseline Armv8 atomics. The root has
// no ELF process auxiliary vector and never unwinds across the C boundary.
unsigned long getauxval(unsigned long key) { (void)key; return 0; }
void rust_eh_personality(void) { net_panic(); }
void _Unwind_Resume(void *state) { (void)state; net_panic(); }
size_t strlen(const char *s) { const char *p = s; while (*p) p++; return p - s; }
int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *x = a, *y = b;
    for (size_t i = 0; i < n; i++) if (x[i] != y[i]) return (int)x[i] - y[i];
    return 0;
}
int bcmp(const void *a, const void *b, size_t n) { return memcmp(a, b, n); }
static uint64_t relay_start_deadline;
static void setup_network(void) {
    uintptr_t va = ring_init((uintptr_t)net_heap, sizeof(net_heap));
    seL4_CPtr frame = alloc_object(seL4_ARM_LargePageObject, 0);
    map(frame, seL4_CapInitThreadVSpace, va, 3);
    seL4_ARM_Page_GetAddress_t phys = seL4_ARM_Page_GetAddress(frame);
    check("socket ring address", phys.error);
    say("VioletOS AArch64 network at 0x"); hex(phys.paddr); say(" slots 0x"); hex(ring_slots()); say("\n");
    relay_start_deadline = ticks_now() + 5 * timer_hz;
}

enum { TCP_STREAM = 1, UDP_DGRAM = 2, RING_CONNECTING = 1, RING_OPEN = 2, RING_CLOSED = 3, RING_ERROR = 4,
       DATAGRAM_BYTES = 1472, DNS_QUEUE = 4, NONBLOCK = 0x800, MSG_NOWAIT = 0x40 };
struct inet_addr { uint16_t family, port; uint32_t ip; unsigned char pad[8]; };
struct dns_reply { struct inet_addr from; size_t size; unsigned char data[DATAGRAM_BYTES]; };
struct inet_socket {
    unsigned refs, kind, flags, next, queued;
    int slot;
    bool shut_read, shut_write, connected, receive_errors;
    struct inet_addr peer;
    struct dns_reply replies[DNS_QUEUE];
};
static const struct file inet_node = {"socket:[inet]", NULL, 0};
static struct inet_socket inet_sockets[128];
// The runtime dispatches syscalls serially. No caller retains this buffer when it sleeps.
static unsigned char net_buf[8192];

static long inet_open(unsigned family, unsigned type, unsigned proto) {
    if (family != 2) return -97;
    unsigned kind = type & 15;
    if ((type & ~(NONBLOCK | 0x80000U | 15)) ||
        (kind != TCP_STREAM && kind != UDP_DGRAM) ||
        (proto && proto != (kind == TCP_STREAM ? 6U : 17U))) return -93;
    int fd = free_fd();
    if (fd < 0) return fd;
    for (unsigned i = 0; i < sizeof(inet_sockets)/sizeof(*inet_sockets); i++) {
        struct inet_socket *s = &inet_sockets[i];
        if (s->refs) continue;
        *s = (struct inet_socket){.refs=1, .kind=kind, .flags=type & NONBLOCK, .slot=-1};
        fds[fd] = (struct descriptor){.inet=s, .file=&inet_node, .cloexec=!!(type & 0x80000)};
        return fd;
    }
    return -23;
}
static void inet_release(struct inet_socket *s) {
    if (!--s->refs && s->slot >= 0) ring_close(s->slot);
}
static long inet_flags(struct inet_socket *s, unsigned op, unsigned flags) {
    if (op == 3) return s->flags | 2;
    s->flags = flags & NONBLOCK;
    return 0;
}
static long inet_again(const struct inet_socket *s, unsigned flags) {
    return ((s->flags & NONBLOCK) || (flags & MSG_NOWAIT)) ? -11 : retry_io(ticks_now() + timer_hz/200);
}
static long read_peer(uintptr_t ptr, size_t len, struct inet_addr *sa) {
    if (len < sizeof(*sa)) return -22;
    if (!transfer(ptr, sa, sizeof(*sa), false)) return -14;
    return sa->family == 2 ? 0 : -97;
}
static long write_peer(const struct inet_addr *sa, uintptr_t ptr, uintptr_t lenptr) {
    if (!ptr) return 0;
    unsigned len;
    if (!transfer(lenptr, &len, sizeof(len), false)) return -14;
    if (len > sizeof(*sa)) len = sizeof(*sa);
    if (!transfer(ptr, (void *)sa, len, true)) return -14;
    len = sizeof(*sa);
    return transfer(lenptr, &len, sizeof(len), true) ? 0 : -14;
}
static long ring_open_error(int err) {
    // The launcher attaches to the physical buffer after reading its boot
    // announcement. Booting the rest of the runtime can already take seconds.
    if (err == -101 && ticks_now() < relay_start_deadline)
        return retry_io(ticks_now() + timer_hz/200);
    return err;
}
static long inet_connect(struct inet_socket *s, uintptr_t ptr, size_t len) {
    struct inet_addr addr;
    long e = read_peer(ptr, len, &addr);
    if (e) return e;
    if (s->kind == UDP_DGRAM) { s->peer = addr; s->connected = true; return 0; }
    if (s->connected) return -106;
    bool fresh = s->slot < 0;
    if (fresh) {
        int slotno = ring_connect(__builtin_bswap32(addr.ip), __builtin_bswap16(addr.port));
#ifdef VIOLET_TRACE
        say("inet connect ip "); hex(__builtin_bswap32(addr.ip)); say(" port "); hex(__builtin_bswap16(addr.port));
        say(" ring "); hex(slotno); say("\n");
#endif
        if (slotno < 0) return ring_open_error(slotno);
        s->slot = slotno; s->peer = addr;
    }
    int err = 0;
    unsigned status = ring_state(s->slot, &err);
    if (status == RING_ERROR) return -(err ? err : 111);
    if (status == RING_CONNECTING) {
        if (s->flags & NONBLOCK) return fresh ? -115 : -114;
        return inet_again(s, 0);
    }
    s->connected = true;
    return 0;
}
static int inet_poll(struct inet_socket *s, int wanted) {
    int events = 0, err = 0;
    if (s->kind == UDP_DGRAM) {
        events = 4 | (s->queued ? 1 : 0);
        if (s->slot >= 0 && ring_avail(s->slot)) events |= 1;
    } else if (s->slot >= 0) {
        unsigned state = ring_state(s->slot, &err);
        if (state == RING_ERROR) return 8 | (wanted & 4);
        if (state == RING_CLOSED) events |= 16 | 1;
        if (state == RING_OPEN && ring_space(s->slot)) events |= 4;
        if (ring_avail(s->slot)) events |= 1;
    }
    return events & (wanted | 24);
}
static long inet_udp_send(struct inet_socket *s, uintptr_t buf, size_t n, unsigned flags, struct inet_addr to) {
    if (n > DATAGRAM_BYTES) return -90;
    if (!transfer(buf, net_buf, n, false)) return -14;
    uint16_t port = __builtin_bswap16(to.port);
    uint32_t ip = __builtin_bswap32(to.ip);
    if (port == 53 && (ip == 0x7f000001 || ip == 0x0a000035)) {
        if (s->queued == DNS_QUEUE) return -11;
        struct dns_reply *reply = &s->replies[(s->next + s->queued) % DNS_QUEUE];
        reply->size = ring_dns(net_buf, n, reply->data, sizeof(reply->data));
        if (!reply->size) return -22;
        reply->from = to; s->queued++;
        return n;
    }
    if (s->slot < 0) {
        int slotno = ring_udp_open();
        if (slotno < 0) return ring_open_error(slotno);
        s->slot = slotno;
    }
    return ring_udp_send(s->slot, ip, port, net_buf, n) ? (long)n : inet_again(s, flags);
}
static long inet_io(struct inet_socket *s, uintptr_t buf, size_t n, bool write,
                    unsigned flags, uintptr_t name, uintptr_t namelen) {
    if (flags & ~(MSG_NOWAIT | 0x4000 | 0x20 | (write ? 0 : 2))) return -95;
    bool peek = flags & 2;
    if (write && s->shut_write) return -32;
    if (!write && s->shut_read) return 0;
    if (s->kind == UDP_DGRAM) {
        struct inet_addr peer = s->peer;
        if (write) {
            if (!name && !s->connected) return -89;
            long e = name ? read_peer(name, namelen, &peer) : 0;
            return e ? e : inet_udp_send(s, buf, n, flags, peer);
        }
        if (!s->queued) {
            struct dns_reply *reply = &s->replies[s->next];
            uint32_t ip; uint16_t port;
            int got = s->slot < 0 ? -11 : ring_udp_recv(s->slot, &ip, &port, reply->data);
            if (got < 0) return inet_again(s, flags);
            reply->size = got;
            reply->from = (struct inet_addr){.family=2, .ip=__builtin_bswap32(ip), .port=__builtin_bswap16(port)};
            s->queued = 1;
        }
        struct dns_reply *reply = &s->replies[s->next];
        size_t received = reply->size;
        if (n > received) n = received;
        long e = write_peer(&reply->from, name, namelen);
        if (e || !transfer(buf, reply->data, n, true)) return -14;
        if (!peek) { s->next = (s->next + 1) % DNS_QUEUE; s->queued--; }
        return flags & 0x20 ? received : n;
    }
    if (s->slot < 0) return -107;
    if (!n) return 0;
    if (n > sizeof(net_buf)) n = sizeof(net_buf);
    int err;
    unsigned state = ring_state(s->slot, &err);
    if (!write && ring_avail(s->slot)) {
        n = ring_recv(s->slot, net_buf, n, peek);
        if (!transfer(buf, net_buf, n, true)) return -14;
        return n;
    }
    if (state == RING_ERROR) return -(err ? err : 104);
    if (state == RING_CLOSED) return write ? -32 : 0;
    if (write && state == RING_OPEN && ring_space(s->slot)) {
        if (!transfer(buf, net_buf, n, false)) return -14;
        return ring_send(s->slot, net_buf, n);
    }
    return inet_again(s, flags);
}
struct inet_msg {
    uintptr_t name; uint32_t namelen, pad;
    uintptr_t iov, iovlen, control, controllen;
    uint32_t flags, padding;
};
static long inet_message(struct inet_socket *s, uintptr_t addr, unsigned flags, bool write) {
    struct inet_msg msg;
    if (!transfer(addr, &msg, sizeof(msg), false)) return -14;
    if (!write && (flags & 0x2000) && s->kind == UDP_DGRAM) return -11; // relay has no queued ICMP errors
    if (msg.iovlen != 1 || (write && msg.controllen)) return -95;
    uintptr_t iov[2];
    if (!transfer(msg.iov, iov, sizeof(iov), false)) return -14;
    long n = inet_io(s, iov[0], iov[1], write, flags, msg.name,
                     write ? msg.namelen : addr + offsetof(struct inet_msg, namelen));
    if (!write && n >= 0) {
        msg.flags = 0; msg.controllen = 0;
        if (!transfer(addr + offsetof(struct inet_msg, controllen), &msg.controllen, sizeof(msg.controllen), true) ||
            !transfer(addr + offsetof(struct inet_msg, flags), &msg.flags, sizeof(msg.flags), true)) return -14;
    }
    return n;
}
static long inet_call(seL4_UserContext *r) {
    if (r->x0 >= MAX_FDS) return -9;
    struct inet_socket *s = fds[r->x0].inet;
    if (!s) return fds[r->x0].file || fds[r->x0].pipe ? -88 : -9;
    switch (r->x8) {
    case 200: { // Host relay sockets use ephemeral local ports.
        struct inet_addr addr;
        long e = read_peer(r->x1, r->x2, &addr);
        return e ? e : (addr.port || addr.ip) ? -99 : 0;
    }
    case 203: return inet_connect(s, r->x1, r->x2);
    case 204: {
        uint32_t ip = 0; uint16_t port = 0;
        if (s->slot >= 0) ring_local(s->slot, &ip, &port);
        struct inet_addr local = {.family=2, .ip=__builtin_bswap32(ip), .port=__builtin_bswap16(port)};
        return write_peer(&local, r->x1, r->x2);
    }
    case 205:
        if (!s->connected) return -107;
        return write_peer(&s->peer, r->x1, r->x2);
    case 206: case 207:
        return inet_io(s, r->x1, r->x2, r->x8 == 206, r->x3, r->x4, r->x5);
    case 208: {
#ifdef VIOLET_TRACE
        say("inet setsockopt level "); hex(r->x1); say(" option "); hex(r->x2); say("\n");
#endif
        int value;
        if (r->x1 != 0 || r->x2 != 11) return -92; // IP_RECVERR
        if (r->x4 < sizeof(value)) return -22;
        if (!transfer(r->x3, &value, sizeof(value), false)) return -14;
        s->receive_errors = !!value;
        return 0;
    }
    case 269: {
        if (r->x2 > 1024) return -22;
        for (unsigned i = 0; i < r->x2; i++) {
            uintptr_t ptr = r->x1 + i * 64; // Linux mmsghdr: 56-byte msghdr, u32 len, padding
            long sent = inet_message(s, ptr, r->x3, true);
            if (caller->state == RETRY_IO && i) {
                caller->state = RUNNING; caller->deadline = 0; caller->reply[8] += 4;
                return i;
            }
            if (caller->state == RETRY_IO || sent < 0) return i ? (long)i : sent;
            uint32_t len = sent;
            if (!transfer(ptr + 56, &len, sizeof(len), true)) return i ? (long)i : -14;
        }
        return r->x2;
    }
    case 209: {
        int err = 0; unsigned len;
        if (r->x1 == 0 && r->x2 == 11) err = s->receive_errors;
        else if (r->x1 != 1 || (r->x2 != 4 && r->x2 != 3)) return -92;
        else if (r->x2 == 3) err = s->kind;
        else if (s->slot >= 0) {
            unsigned state = ring_state(s->slot, &err);
            if (state == RING_OPEN) s->connected = true;
        }
        if (!transfer(r->x4, &len, sizeof(len), false)) return -14;
        if (len > sizeof(err)) len = sizeof(err);
        return transfer(r->x3, &err, len, true) && transfer(r->x4, &len, sizeof(len), true) ? 0 : -14;
    }
    case 210:
        if (r->x1 > 2) return -22;
        if (s->slot < 0 && !s->connected) return -107;
        if (r->x1 != 1) s->shut_read = true;
        if (r->x1 && s->slot >= 0) { s->shut_write = true; ring_shutdown(s->slot); }
        return 0;
    case 211: case 212: return inet_message(s, r->x1, r->x2, r->x8 == 211);
    default: return -92;
    }
}
