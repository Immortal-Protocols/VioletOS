// QEMU scanout and touch bridge. The app sees the same surface, present and
// event records as ARMv7. The viewer reads only pixels the guest has presented.
enum { DISPLAY_W = 720, DISPLAY_H = 1600, FRAME_BYTES = DISPLAY_W * DISPLAY_H * 4,
       SURFACE_BYTES = (FRAME_BYTES + 65535) & -65536, INPUT_SLOTS = 64 };
struct display_bridge {
    uint32_t pixels[DISPLAY_H][DISPLAY_W];
    volatile uint32_t dirty[1024]; // DIRT, first row, past last row, published, consumed
    volatile uint32_t produced, consumed;
    volatile uint8_t reports[INPUT_SLOTS][6]; // same touch reports as the existing SDL viewer
};
static struct display_bridge display_bridge __attribute__((aligned(16384)));
static int foreground_pid;
static const struct file sys_node;
static long open_gui_control(unsigned flags);
static long gui_io(struct descriptor *, uintptr_t, size_t, bool);
static bool launcher_owns_exit(struct process *);
static const struct file screen_node = {"/dev/violet/screen", NULL, 0};
static const struct file audio_node = {"/dev/violet/audio", NULL, 0};
static long configure_sound(struct descriptor *, uint64_t, uintptr_t);
static const struct file events_node = {"/dev/violet/events", NULL, 0};

static const struct file *display_node(const char *s) {
    if (same(s, "/dev/violet/sys")) return &sys_node;
    if (same(s, audio_node.name)) return &audio_node;
    if (same(s, screen_node.name)) return &screen_node;
    if (same(s, events_node.name)) return &events_node;
    return NULL;
}

// Each descriptor keeps its own guest surface base in pos. Reserve both
// buffers together, as adjacent shared mappings must name the same pixels.
// Actual pages are committed only when the app writes them.
static long display_mmap(seL4_UserContext *r) {
    uint64_t len = r->x1, offset = r->x5;
    if ((r->x3 & 0xf) != 1 || (r->x3 & 16) || (r->x2 & ~3UL) || !len ||
        offset % PAGE || offset >= 2 * SURFACE_BYTES || len > 2 * SURFACE_BYTES - offset)
        return -22;
    struct descriptor *d = &fds[r->x4];
    if (!foreground_pid) foreground_pid = active_process->pid;
    if (!d->pos || !region_at(d->pos + offset)) {
        uintptr_t lo = next_map;
        if (!add_region(lo, lo + 2 * SURFACE_BYTES, r->x2)) return -12;
        next_map += 2 * SURFACE_BYTES;
        d->pos = lo;
    }
    return d->pos + offset;
}

static long screen_ioctl(uint64_t fd, uint64_t cmd, uintptr_t addr) {
    if (fd < MAX_FDS && fds[fd].file == &audio_node) return configure_sound(&fds[fd], cmd, addr);
    if (fd >= MAX_FDS || fds[fd].file != &screen_node) return -25;
    if (cmd == 0x5601) {
        uint32_t info[] = {DISPLAY_W, DISPLAY_H, DISPLAY_W * 4, 1, 2, SURFACE_BYTES};
        return transfer(addr, info, sizeof(info), true) ? 0 : -14;
    }
    if (cmd == 0x5604) {
        uint64_t now = ticks_now() - start_ticks;
        uint64_t frame = now / timer_hz * 60 + now % timer_hz * 60 / timer_hz + 1;
        uint64_t tick = frame / 60 * timer_hz + (frame % 60 * timer_hz + 59) / 60;
        struct { uint32_t seq, period_us; uint64_t time_us; } stamp = {frame, 16667, frame * 1000000 / 60};
        if (addr && !transfer(addr, &stamp, sizeof(stamp), true)) return -14;
        caller->state = SLEEPING;
        caller->deadline = start_ticks + tick;
        return 0;
    }
    return -25;
}

static long screen_present(struct descriptor *d, uintptr_t addr, size_t n) {
    uint32_t r[6];
    if (n != sizeof(r) || !d->pos) return -22;
    if (!transfer(addr, r, sizeof(r), false)) return -14;
    unsigned x=r[0], y=r[1], width=r[2], height=r[3];
    if (r[4] > 1 || x > DISPLAY_W || y > DISPLAY_H || width > DISPLAY_W-x || height > DISPLAY_H-y)
        return -22;
    if (!width || !height || foreground_pid != active_process->pid) return sizeof(r);
    uintptr_t src = d->pos + r[4] * SURFACE_BYTES + (y * DISPLAY_W + x) * 4;
    if (width == DISPLAY_W) {
        if (!transfer(src, display_bridge.pixels[y], width * height * 4, false)) return -14;
    } else {
        for (unsigned row = 0; row < height; row++, src += DISPLAY_W*4)
            if (!transfer(src, &display_bridge.pixels[y+row][x], width * 4, false)) return -14;
    }
    volatile uint32_t *dirty = display_bridge.dirty;
    unsigned bottom = y + height;
    if (dirty[3] != dirty[4]) {
        if (dirty[1] < y) y = dirty[1];
        if (dirty[2] > bottom) bottom = dirty[2];
    }
    dirty[0] = 0x54524944;
    dirty[1] = y; dirty[2] = bottom;
    __atomic_store_n(&dirty[3], dirty[3]+1, __ATOMIC_RELEASE);
    return sizeof(r);
}

static long read_touch(struct descriptor *d, uintptr_t addr, size_t n) {
    struct { uint64_t time_us; uint16_t kind, code; int32_t x, y, value; } e = {0};
    static bool down;
    static uint32_t x, y;
    if (!n) return 0;
    if (n < sizeof(e)) return -22;
    bool front = foreground_pid == active_process->pid;
    size_t state = front ? 2 : 1;
    if (d->pos != state) {
        e.kind = 7; // LIFECYCLE
        e.value = front ? 1 : 2;
        if (!transfer(addr, &e, sizeof(e), true)) return -14;
        d->pos = state;
        return sizeof(e);
    }
    if (!front) return d->writer ? -11 : retry_io(ticks_now() + timer_hz / 40);
    unsigned tail = display_bridge.consumed;
    if (tail == __atomic_load_n(&display_bridge.produced, __ATOMIC_ACQUIRE))
        return d->writer ? -11 : retry_io(ticks_now() + timer_hz / 200);
    volatile uint8_t *report = display_bridge.reports[tail % INPUT_SLOTS];
    bool next = report[0] != 0;
    uint32_t nx = next ? (report[1] << 4) | (report[3] >> 4) : x;
    uint32_t ny = next ? (report[2] << 4) | (report[3] & 15) : y;
    e.kind = next ? (down ? 2 : 1) : 3;
    e.x = nx; e.y = ny; e.value = next;
    uint64_t ticks = ticks_now() - start_ticks;
    e.time_us = ticks / timer_hz * 1000000 + ticks % timer_hz * 1000000 / timer_hz;
    if (!transfer(addr, &e, sizeof(e), true)) return -14;
    down = next; x = nx; y = ny;
    __atomic_store_n(&display_bridge.consumed, tail+1, __ATOMIC_RELEASE);
    return sizeof(e);
}
