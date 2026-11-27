// Raw pseudo-terminals for browser presentation. The slave carries bytes and
// reports its pane geometry to the upstream terminal frontend.
enum { PTY_COUNT = 4, PTY_BYTES = 16384 };
struct tty_queue { unsigned head, size; unsigned char bytes[PTY_BYTES]; };
struct tty_pair {
    struct file node[2]; // master, slave
    unsigned refs[2];
    char pathname[12];
    bool locked;
    uint16_t window[4];
    unsigned char termios[36];
    struct tty_queue input[2];
};
static struct tty_pair terminals[PTY_COUNT];
static struct tty_pair *tty_of(const struct file *node, unsigned *side) {
    if ((uintptr_t)node < (uintptr_t)terminals || (uintptr_t)node >= (uintptr_t)(terminals + PTY_COUNT)) return NULL;
    struct tty_pair *p = &terminals[((uintptr_t)node - (uintptr_t)terminals) / sizeof(*p)];
    if (node == &p->node[0]) { *side = 0; return p; }
    if (node == &p->node[1]) { *side = 1; return p; }
    return NULL;
}
static int tty_open(const char *path, unsigned flags) {
    unsigned side = !same(path, "/dev/ptmx");
    for (unsigned i = 0; i < PTY_COUNT; i++) {
        struct tty_pair *p = &terminals[i];
        if (!side) {
            if (p->refs[0] || p->refs[1]) continue;
            *p = (struct tty_pair){.locked=true, .window={24,80,0,0}};
            memcpy(p->pathname, "/dev/pts/0", 11); p->pathname[9] += i;
            p->node[0].name = "/dev/ptmx"; p->node[1].name = p->pathname;
        } else {
            if (!p->refs[0] || !same(path, p->pathname)) continue;
            if (p->locked) return -5;
        }
        int fd = install_regular(&p->node[side], flags);
        if (fd >= 0) p->refs[side]++;
        return fd;
    }
    return side ? -2 : -28;
}
static bool tty_path(const char *s) {
    const char *prefix = "/dev/pts/";
    if (same(s, "/dev/ptmx")) return true;
    while (*prefix && *s == *prefix) { s++; prefix++; }
    return !*prefix && *s >= '0' && *s < '0' + PTY_COUNT && !s[1];
}
static void tty_ref(const struct file *node, int delta) {
    unsigned side;
    struct tty_pair *p = tty_of(node, &side);
    if (p) p->refs[side] += delta;
}
static long tty_ioctl(struct tty_pair *p, unsigned side, unsigned cmd, uintptr_t addr) {
    switch (cmd) {
    case 0x80045430: { // TIOCGPTN
        unsigned index = p - terminals;
        if (side) return -25;
        return transfer(addr, &index, sizeof(index), true) ? 0 : -14;
    }
    case 0x40045431: { // TIOCSPTLCK
        int lock;
        if (side) return -25;
        if (!transfer(addr, &lock, sizeof(lock), false)) return -14;
        p->locked = !!lock; return 0;
    }
    case 0x5413: case 0x5414: // TIOCGWINSZ / TIOCSWINSZ
        return transfer(addr, p->window, sizeof(p->window), cmd == 0x5413) ? 0 : -14;
    case 0x5401: // TCGETS
        return transfer(addr, p->termios, sizeof(p->termios), true) ? 0 : -14;
    case 0x5402: case 0x5403: case 0x5404: {
        unsigned attr[9];
        if (!transfer(addr, attr, sizeof(attr), false)) return -14;
        if (attr[3] & (1 | 2 | 8)) return -95; // raw mode only
        memcpy(p->termios, attr, sizeof(attr));
        if (cmd == 0x5404) p->input[side].head = p->input[side].size = 0;
        return 0;
    }
    default: return -25;
    }
}
static int tty_poll(struct tty_pair *p, unsigned side, int requested) {
    int events = (p->input[side].size ? 1 : 0) | (p->input[1-side].size < PTY_BYTES ? 4 : 0);
    if (!p->refs[1-side]) events |= 16;
    return events & (requested | 16);
}
static long tty_stream(struct tty_pair *p, unsigned side, struct descriptor *d, uintptr_t addr, size_t len, bool write) {
    if (!len) return 0;
    struct tty_queue *q = &p->input[write ? 1-side : side];
    if (!p->refs[1-side] && (write || !q->size)) return side ? 0 : -5;
    size_t ready = write ? PTY_BYTES - q->size : q->size;
    if (!ready) return d->cursor->flags & 0x800 ? -11 : retry_io(ticks_now() + timer_hz/200);
    if (len > ready) len = ready;
    unsigned offset = (q->head + (write ? q->size : 0)) % PTY_BYTES;
    size_t chunk = PTY_BYTES - offset;
    if (chunk > len) chunk = len;
    if (!transfer(addr, q->bytes + offset, chunk, !write) ||
        !transfer(addr + chunk, q->bytes, len - chunk, !write)) return -14;
    if (write) q->size += len;
    else { q->head = (q->head + len) % PTY_BYTES; q->size -= len; }
    return len;
}
