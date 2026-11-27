// A Linux event counter is an open description shared by dup, fork and SCM_RIGHTS.
struct event_counter {
    struct file node;
    uint64_t value;
    unsigned refs, flags;
};
static struct event_counter event_counters[128];
static struct event_counter *event_of(const struct file *file) {
    uintptr_t addr = (uintptr_t)file;
    return addr >= (uintptr_t)event_counters && addr < (uintptr_t)(event_counters + 128) ? (struct event_counter *)file : NULL;
}
static long event_open(unsigned value, unsigned flags) {
    if (flags & ~(1u | 0x800u | 0x80000u)) return -22;
    int fd = free_fd();
    if (fd < 0) return fd;
    for (unsigned i = 0; i < 128; i++) {
        struct event_counter *e = &event_counters[i];
        if (e->refs) continue;
        *e = (struct event_counter){.node = {"anon_inode:[eventfd]", NULL, 0}, .value = value, .refs = 1, .flags = flags & 0x801};
        fds[fd] = (struct descriptor){.file = &e->node, .cloexec = !!(flags & 0x80000)};
        return fd;
    }
    return -23;
}
static long event_rw(struct event_counter *e, uintptr_t addr, size_t length, bool write) {
    if (length < sizeof(uint64_t)) return -22;
    if (write) {
        uint64_t add;
        if (!transfer(addr, &add, 8, false)) return -14;
        if (add == UINT64_MAX) return -22;
        if (add <= UINT64_MAX - 1 - e->value) { e->value += add; wake_pollers(e); return 8; }
    } else if (e->value) {
        uint64_t value = e->flags & 1 ? 1 : e->value;
        if (!transfer(addr, &value, 8, true)) return -14;
        e->value -= value; wake_pollers(e); return 8;
    }
    return e->flags & 0x800 ? -11 : retry_io(ticks_now() + timer_hz / 500);
}
