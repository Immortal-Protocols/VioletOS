// AF_UNIX stream sockets. Descriptor references travel with the first byte
// of a sendmsg, so partial reads and writes cannot duplicate a reference.
enum { LOCAL_ENDPOINTS = 512, LOCAL_BYTES = 64 * 1024, LOCAL_RIGHTS = 2048, RIGHTS_PER_MSG = 253 };
struct local_socket {
    struct file node;
    unsigned refs, flags;
    int pid, peer_pid;
    struct local_socket *peer, *listener;
    struct tmp_node *bound;
    unsigned char name[110], peer_name[110];
    unsigned name_len, peer_len, backlog;
    bool connected, listening, read_shut, write_shut;
    uint64_t head, tail;
    unsigned first_right, last_right, queued_refs;
    uint16_t resident;
};
static struct local_socket local_sockets[LOCAL_ENDPOINTS];
struct passed_right {
    uint64_t at;
    struct descriptor descriptor;
    unsigned next;
};
static struct passed_right passed_rights[LOCAL_RIGHTS + 1];
static unsigned free_right, rights_high, rights_used;
static void reference_fd(struct descriptor *d);
static void drop_descriptor(struct descriptor *d);

static struct local_socket *local_of(const struct file *node);
static unsigned char *local_buffer(struct local_socket *s, size_t offset) {
    uintptr_t base = 0x2000000000ULL + (s - local_sockets) * LOCAL_BYTES;
    unsigned page = offset / PAGE;
    if (!(s->resident & (1u << page))) {
        seL4_CPtr frame = alloc_object(seL4_ARM_SmallPageObject, 0);
        map(frame, seL4_CapInitThreadVSpace, base + page * PAGE, 3);
        s->resident |= 1u << page;
    }
    return (unsigned char *)(base + offset);
}
static void local_push_right(struct local_socket *s, uint64_t at, struct descriptor descriptor) {
    unsigned id = free_right;
    if (id)
        free_right = passed_rights[id].next;
    else
        id = ++rights_high;
    ++rights_used;
    passed_rights[id] = (struct passed_right){.at = at, .descriptor = descriptor};
    reference_fd(&passed_rights[id].descriptor);
    struct local_socket *passed_socket = local_of(descriptor.file);
    if (passed_socket) ++passed_socket->queued_refs;
    if (s->last_right)
        passed_rights[s->last_right].next = id;
    else
        s->first_right = id;
    s->last_right = id;
}
static struct descriptor local_pop_right(struct local_socket *s) {
    unsigned id = s->first_right;
    struct passed_right *right = &passed_rights[id];
    s->first_right = right->next;
    if (!s->first_right)
        s->last_right = 0;
    struct local_socket *passed_socket = local_of(right->descriptor.file);
    if (passed_socket) --passed_socket->queued_refs;
    right->next = free_right;
    free_right = id;
    --rights_used;
    return right->descriptor;
}
static struct local_socket *local_of(const struct file *node) {
    if ((uintptr_t)node < (uintptr_t)local_sockets || (uintptr_t)node >= (uintptr_t)(local_sockets + LOCAL_ENDPOINTS))
        return NULL;
    return (struct local_socket *)node;
}
static struct local_socket *local_new(unsigned flags) {
    for (unsigned i = 0; i < LOCAL_ENDPOINTS; i++)
        if (!local_sockets[i].refs) {
            struct local_socket *s = &local_sockets[i];
            memset(s, 0, offsetof(struct local_socket, resident));
            s->node.name = "socket:[unix]";
            s->refs = 1;
            s->pid = active_process->pid;
            s->flags = flags & 0x800;
            s->name[0] = 1;
            s->name_len = 2;
            return s;
        }
    return NULL;
}
// Queued SCM_RIGHTS may refer to the receiving socket or form longer cycles.
// Trace from external descriptor owners and from listeners to pending accepts.
static void local_release(struct local_socket *s);
static bool collecting_sockets;
static void local_collect_cycles(void) {
    if (collecting_sockets)
        return;
    collecting_sockets = true;
    bool marked[LOCAL_ENDPOINTS] = {0};
    unsigned short pending[LOCAL_ENDPOINTS];
    unsigned count = 0;
    for (unsigned i = 0; i < LOCAL_ENDPOINTS; ++i) {
        struct local_socket *s = &local_sockets[i];
        if (s->refs > s->queued_refs + (s->listener != NULL)) {
            marked[i] = true;
            pending[count++] = i;
        }
    }
    while (count) {
        struct local_socket *s = &local_sockets[pending[--count]];
        for (unsigned id = s->first_right; id; id = passed_rights[id].next) {
            struct local_socket *target = local_of(passed_rights[id].descriptor.file);
            if (target && !marked[target - local_sockets]) {
                unsigned idx = target - local_sockets;
                marked[idx] = true;
                pending[count++] = idx;
            }
        }
        if (s->listening)
            for (unsigned i = 0; i < LOCAL_ENDPOINTS; ++i)
                if (!marked[i] && local_sockets[i].refs && local_sockets[i].listener == s) {
                    marked[i] = true;
                    pending[count++] = i;
                }
    }
    // Hold garbage endpoints until all unreachable references have been removed.
    for (unsigned i = 0; i < LOCAL_ENDPOINTS; ++i)
        if (!marked[i] && local_sockets[i].refs) {
            ++local_sockets[i].refs;
            pending[count++] = i;
        }
    for (unsigned i = 0; i < count; ++i) {
        struct local_socket *s = &local_sockets[pending[i]];
        while (s->first_right) {
            struct descriptor descriptor = local_pop_right(s);
            drop_descriptor(&descriptor);
        }
    }
    while (count)
        local_release(&local_sockets[pending[--count]]);
    collecting_sockets = false;
}
static void local_release(struct local_socket *s) {
    if (--s->refs) {
        if (s->queued_refs && s->refs == s->queued_refs + (s->listener != NULL))
            local_collect_cycles();
        return;
    }
    if (s->peer) {
        s->peer->peer = NULL;
        wake_pollers(s->peer);
    }
    if (s->bound) {
        --s->bound->opens;
        tmp_collect(s->bound);
    }
    while (s->first_right) {
        struct descriptor d = local_pop_right(s);
        drop_descriptor(&d);
    }
    if (!s->listening)
        return;
    for (unsigned i = 0; i < LOCAL_ENDPOINTS; i++)
        if (local_sockets[i].refs && local_sockets[i].listener == s) {
            local_sockets[i].listener = NULL;
            local_release(&local_sockets[i]);
        }
}
static void local_pair(struct local_socket *x, struct local_socket *y) {
    x->peer = y;
    y->peer = x;
    x->connected = y->connected = true;
    x->peer_pid = y->pid;
    y->peer_pid = x->pid;
    x->peer_len = y->name_len;
    y->peer_len = x->name_len;
    memcpy(x->peer_name, y->name, y->name_len);
    memcpy(y->peer_name, x->name, x->name_len);
}
static long local_open(unsigned type, unsigned protocol) {
    if ((type & ~(0x80000 | 0x800)) != 1 || protocol)
        return -93;
    int fd = free_fd();
    if (fd < 0)
        return fd;
    struct local_socket *s = local_new(type);
    if (!s)
        return -23;
    fds[fd] = (struct descriptor){.file = &s->node, .cloexec = !!(type & 0x80000)};
    return fd;
}
static long local_socketpair(seL4_UserContext *r) {
    if (r->x0 != 1)
        return -97;
    int pair[2];
    pair[0] = local_open(r->x1, r->x2);
    if (pair[0] < 0)
        return pair[0];
    pair[1] = local_open(r->x1, r->x2);
    if (pair[1] < 0 || !transfer(r->x3, pair, sizeof(pair), true)) {
        drop_descriptor(&fds[pair[0]]);
        if (pair[1] >= 0)
            drop_descriptor(&fds[pair[1]]);
        return pair[1] < 0 ? pair[1] : -14;
    }
    local_pair(local_of(fds[pair[0]].file), local_of(fds[pair[1]].file));
    return 0;
}
static long local_wait(const struct local_socket *s, unsigned flags) {
    return ((flags & 0x40) || (s->flags & 0x800)) ? -11 : retry_io(ticks_now() + timer_hz / 500);
}
struct local_msg {
    uint64_t name;
    uint32_t namelen, pad;
    uint64_t iov, iovlen, control, controllen;
    uint32_t flags, pad2;
};
struct local_cmsg {
    uint64_t len;
    int level, type;
};
struct local_iov {
    uintptr_t address;
    size_t length;
};
static long local_receive_rights(struct local_socket *s, uint64_t first, uint64_t end, struct local_msg *msg,
                                 unsigned flags) {
    int received[RIGHTS_PER_MSG];
    size_t n = 0;
    size_t fd_capacity = msg && msg->control && msg->controllen > 16 ? (msg->controllen - 16) / 4 : 0;
    for (unsigned id = s->first_right; id;) {
        struct passed_right *right = &passed_rights[id];
        if (right->at < first || right->at >= end)
            break;
        id = right->next;
        struct descriptor descriptor = flags & 2 ? right->descriptor : local_pop_right(s);
        int fd = n < fd_capacity ? free_fd() : -24;
        if (fd >= 0) {
            fds[fd] = descriptor;
            fds[fd].cloexec = !!(flags & 0x40000000);
            if (flags & 2)
                reference_fd(&fds[fd]);
            received[n++] = fd;
        } else {
            if (msg)
                msg->flags |= 8;
            if (!(flags & 2))
                drop_descriptor(&descriptor);
        }
    }
    if (!msg)
        return 0;
    size_t capacity = msg->controllen;
    msg->controllen = 0;
    if (!n)
        return 0;
    struct local_cmsg c = {16 + 4 * n, 1, 1};
    if (!transfer(msg->control, &c, sizeof(c), true) || !transfer(msg->control + 16, received, n * 4, true)) {
        for (size_t i = 0; i < n; i++)
            drop_descriptor(&fds[received[i]]);
        return -14;
    }
    msg->controllen = (c.len + 7) & ~7ULL;
    if (msg->controllen > capacity)
        msg->controllen = capacity;
    return 0;
}
static long local_io(struct local_socket *s, struct local_iov *iov, unsigned count, bool write, unsigned flags,
                     struct local_msg *msg) {
    int outgoing[RIGHTS_PER_MSG];
    unsigned nrights = 0;
    if (!s->connected)
        return -107;
    if (write && (!s->peer || s->write_shut || s->peer->read_shut))
        return -32;
    if (!write && s->read_shut)
        return 0;
    struct local_socket *queue = write ? s->peer : s;
    size_t available = queue->tail - queue->head;
    if (write)
        available = LOCAL_BYTES - available;
    uint64_t start = write ? queue->tail : queue->head;
    if (write && msg && msg->controllen) {
        if (msg->controllen > 65536 || !msg->control)
            return -22;
        uint64_t at = 0;
        while (at + sizeof(struct local_cmsg) <= msg->controllen) {
            struct local_cmsg c;
            if (!transfer(msg->control + at, &c, sizeof(c), false))
                return -14;
            if (c.len < 16 || c.len > msg->controllen - at || c.level != 1 || c.type != 1)
                return -22;
            size_t nr = (c.len - 16) / 4;
            if (nrights + nr > RIGHTS_PER_MSG)
                return -22;
            if (!transfer(msg->control + at + 16, outgoing + nrights, 4 * nr, false))
                return -14;
            nrights += nr;
            at += (c.len + 7) & ~7ULL;
        }
        for (unsigned i = 0; i < nrights; i++)
            if ((unsigned)outgoing[i] >= MAX_FDS || (!fds[outgoing[i]].file && !fds[outgoing[i]].pipe))
                return -9;
        if (LOCAL_RIGHTS - rights_used < nrights)
            return -105;
    }
    // Stop before the next control bundle. Each recvmsg delivers at most one
    // SCM_RIGHTS list, including when several sendmsg calls are already buffered.
    if (!write && available && s->first_right) {
        unsigned id = s->first_right;
        uint64_t first = passed_rights[id].at;
        do {
            id = passed_rights[id].next;
        } while (id && passed_rights[id].at == first);
        if (id && passed_rights[id].at - start < available)
            available = passed_rights[id].at - start;
    }
    size_t total = 0;
    for (unsigned i = 0; i < count; i++) {
        if (!total && !available && iov[i].length) {
            if (!write && (!s->peer || s->peer->write_shut))
                break;
            return local_wait(s, flags);
        }
        size_t len = iov[i].length;
        if (len > available - total)
            len = available - total;
        size_t done = 0;
        while (done < len) {
            size_t index = (start + total) % LOCAL_BYTES;
            size_t chunk = len - done;
            if (chunk > PAGE - index % PAGE)
                chunk = PAGE - index % PAGE;
            if (!transfer(iov[i].address + done, local_buffer(queue, index), chunk, !write))
                return -14;
            done += chunk;
            total += chunk;
        }
        if (len != iov[i].length)
            break;
    }
    if (write) {
        if (total)
            for (unsigned i = 0; i < nrights; i++)
                local_push_right(queue, start, fds[outgoing[i]]);
        queue->tail += total;
        if (total && start == queue->head) wake_pollers(queue);
    } else {
        if (msg)
            msg->flags = 0;
        long err = local_receive_rights(s, start, start + total, msg, flags);
        if (!(flags & 2)) {
            bool full = queue->tail - start == LOCAL_BYTES;
            queue->head += total;
            if (full && total && s->peer) wake_pollers(s->peer);
        }
        if (err)
            return err;
    }
    return total;
}
static long local_stream(struct local_socket *s, uintptr_t address, size_t length, bool write, unsigned flags) {
    struct local_iov iov = {address, length};
    return local_io(s, &iov, 1, write, flags, NULL);
}
static long local_address(uintptr_t addr, uintptr_t lenp, const unsigned char *data, unsigned len) {
    if (!addr)
        return 0;
    uint32_t cap;
    if (!transfer(lenp, &cap, 4, false) || !transfer(addr, (void *)data, len < cap ? len : cap, true) ||
        !transfer(lenp, &len, 4, true))
        return -14;
    return 0;
}
static long local_bind_connect(struct local_socket *s, uintptr_t addr, size_t len, bool bind) {
    unsigned char name[111] = {0};
    if (len <= 2 || len > 110)
        return -22;
    if (!transfer(addr, name, len, false))
        return -14;
    if (name[0] != 1 || name[1])
        return -22;
    bool path = name[2] != 0;
    if (path)
        len = path_length((char *)name + 2) + 3;
    if (len > 110)
        return -36;
    struct tmp_node *node = path ? tmp_lookup((char *)name + 2) : NULL;
    if (bind && (s->name_len != 2 || node))
        return node ? -98 : -22;
    struct local_socket *target = NULL;
    for (unsigned i = 0; i < LOCAL_ENDPOINTS; i++) {
        struct local_socket *x = &local_sockets[i];
        if (!x->refs || x->name_len != len)
            continue;
        bool equal = path ? node && node == x->bound : true;
        for (unsigned j = 2; equal && j < len; j++)
            if (x->name[j] != name[j])
                equal = false;
        if (equal) {
            target = x;
            break;
        }
    }
    if (bind) {
        if (target)
            return -98;
        if (path) {
            int err = tmp_parent((char *)name + 2);
            if (err)
                return err;
            if (!land_parent((char *)name + 2, LL_BIND)) return -13;
            s->bound = tmp_make((char *)name + 2, TMP_SOCK | 0755, true);
            if (!s->bound)
                return -28;
            ++s->bound->opens;
        }
        memcpy(s->name, name, len);
        s->name_len = len;
        return 0;
    }
    if (s->connected)
        return -106;
    if (!target || !target->listening)
        return path && !node ? -2 : -111;
    unsigned waiting = 0;
    for (unsigned i = 0; i < LOCAL_ENDPOINTS; i++)
        waiting += local_sockets[i].refs && local_sockets[i].listener == target;
    if (waiting >= target->backlog)
        return local_wait(s, 0);
    struct local_socket *accepted = local_new(0);
    if (!accepted)
        return -105;
    accepted->pid = target->pid;
    accepted->listener = target;
    accepted->name_len = target->name_len;
    memcpy(accepted->name, target->name, target->name_len);
    local_pair(s, accepted);
    wake_pollers(target);
    return 0;
}
static long local_accept(struct local_socket *s, seL4_UserContext *r) {
    if (!s->listening)
        return -22;
    unsigned flags = r->x8 == 242 ? r->x3 : 0;
    if (flags & ~(0x800u | 0x80000u))
        return -22;
    for (unsigned i = 0; i < LOCAL_ENDPOINTS; i++) {
        struct local_socket *x = &local_sockets[i];
        if (!x->refs || x->listener != s)
            continue;
        int fd = free_fd();
        if (fd < 0)
            return fd;
        long err = local_address(r->x1, r->x2, x->peer_name, x->peer_len);
        if (err)
            return err;
        x->flags = flags & 0x800;
        x->listener = NULL;
        fds[fd] = (struct descriptor){.file = &x->node, .cloexec = !!(flags & 0x80000)};
        return fd;
    }
    return local_wait(s, 0);
}
static unsigned local_poll(struct local_socket *s, unsigned events) {
    if (s->listening) {
        for (unsigned i = 0; i < LOCAL_ENDPOINTS; i++)
            if (local_sockets[i].refs && local_sockets[i].listener == s)
                return events & 1;
        return 0;
    }
    unsigned result = 0;
    if (!s->peer || s->peer->write_shut || s->read_shut || s->head != s->tail)
        result |= events & 1;
    if (!s->peer)
        result |= 16;
    if (!s->peer || s->peer->tail - s->peer->head < LOCAL_BYTES)
        result |= events & 4;
    return result;
}
static long local_call(struct local_socket *s, seL4_UserContext *r) {
    switch (r->x8) {
    case 200:
    case 203:
        return local_bind_connect(s, r->x1, r->x2, r->x8 == 200);
    case 201:
        if (s->connected || s->name_len == 2)
            return -22;
        s->listening = true;
        s->backlog = r->x1 && r->x1 < LOCAL_ENDPOINTS ? r->x1 : 1;
        return 0;
    case 202:
    case 242:
        return local_accept(s, r);
    case 204:
        return local_address(r->x1, r->x2, s->name, s->name_len);
    case 205:
        return s->connected ? local_address(r->x1, r->x2, s->peer_name, s->peer_len) : -107;
    case 206:
    case 207:
        return local_stream(s, r->x1, r->x2, r->x8 == 206, r->x3);
    case 208:
        if (r->x1 != 1 || (r->x2 != 7 && r->x2 != 8))
            return -92;
        int requested;
        return r->x4 < 4 ? -22 : transfer(r->x3, &requested, 4, false) ? 0 : -14;
    case 209: {
        if (r->x1 != 1)
            return -92;
        int values[3] = {0};
        unsigned n = 4, cap;
        if (r->x2 == 17) {
            values[0] = s->peer_pid;
            values[1] = values[2] = 1000;
            n = 12;
        } else if (r->x2 == 7 || r->x2 == 8)
            values[0] = LOCAL_BYTES;
        else if (r->x2 == 3 || r->x2 == 39)
            values[0] = 1;
        else if (r->x2 != 4)
            return -92;
        if (!transfer(r->x4, &cap, 4, false))
            return -14;
        if (cap < n)
            n = cap;
        return transfer(r->x3, values, n, true) && transfer(r->x4, &n, 4, true) ? 0 : -14;
    }
    case 210:
        if (r->x1 > 2 || !s->connected)
            return -22;
        if (r->x1 != 1)
            s->read_shut = true;
        if (r->x1 != 0)
            s->write_shut = true;
        wake_pollers(s);
        if (s->peer) wake_pollers(s->peer);
        return 0;
    case 211:
    case 212: {
        struct local_msg m;
        if (!transfer(r->x1, &m, sizeof(m), false))
            return -14;
        struct local_iov iov[64];
        if (m.iovlen > 64)
            return -90;
        if (!transfer(m.iov, iov, m.iovlen * sizeof(*iov), false))
            return -14;
        bool write = r->x8 == 211;
        long count = local_io(s, iov, m.iovlen, write, r->x2, &m);
        if (!write && count >= 0 && caller->state == RUNNING) {
            m.namelen = 0;
            if (!transfer(r->x1, &m, sizeof(m), true))
                return -14;
        }
        return count;
    }
    default:
        return -38;
    }
}
