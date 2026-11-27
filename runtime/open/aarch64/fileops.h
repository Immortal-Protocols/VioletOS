// Linux descriptor operations for our immutable file image, console and pipes.
// There is no ptrace facility on the native target.
static const struct file process_status = {"/proc/self/status", (const unsigned char *)"TracerPid:\t0\n", sizeof("TracerPid:\t0\n") - 1};
static const struct file null_node = {"/dev/null", NULL, 0};
static const struct file zero_node = {"/dev/zero", NULL, 0};
static const struct file urandom_node = {"/dev/urandom", NULL, 0}, random_node = {"/dev/random", NULL, 0};
static long read_entropy(uintptr_t dest, size_t count, bool nonblock) {
    size_t avail = entropy_available();
    if (!count) return 0;
    if (!avail) return !nonblock && entropy_channel.online ? retry_io(ticks_now() + timer_hz/200) : -11;
    if (count > avail) count = avail;
    size_t done = 0;
    while (done < count) {
        unsigned char bytes[256];
        size_t n = count - done;
        if (n > sizeof(bytes)) n = sizeof(bytes);
        if (!random_bytes(bytes, n) || !transfer(dest + done, bytes, n, true)) return done ? (long)done : -14;
        done += n;
    }
    return done;
}
static const struct file *char_device(const char *path) {
    if (same(path, urandom_node.name)) return &urandom_node;
    if (same(path, random_node.name)) return &random_node;
    if (same(path, null_node.name)) return &null_node;
    return same(path, zero_node.name) ? &zero_node : NULL;
}
static struct file overcommit = {"/proc/sys/vm/overcommit_memory", (const unsigned char *)"1\n", 2};

static const struct file ptmx_node = {"/dev/ptmx", NULL, 0};
static void tty_ref(const struct file *, int);
static const struct file task_dir = {"/proc/self/task", NULL, 0};
#include "packed_dirs.h"
#include "landlock.h"
#include "tmpops.h"
#include "pty.h"
#include "localsocket.h"
#include "eventfd.h"
static bool land_directory(const struct file *f) {
    struct tmp_node *n = tmp_node_of(f);
    return f == &task_dir || is_packed_dir(f) || (n && (n->mode & 0170000) == TMP_DIR);
}
static const struct file proc_exe_link = {"/proc/self/exe", NULL, 0};
static const struct file proc_maps_entry = {"/proc/self/maps", NULL, 0};
static const struct file *runtime_dirent(size_t index, unsigned *type) {
    static const struct file *const fixed[] = {
        &null_node, &zero_node, &ptmx_node, &sys_node, &screen_node, &events_node, &audio_node,
        &process_status, &overcommit, &proc_exe_link, &proc_maps_entry, &random_node, &urandom_node
    };
    _Static_assert(RUNTIME_DIRENTS == sizeof(fixed)/sizeof(*fixed) + PTY_COUNT, "virtual directory entries");
    if (index < sizeof(fixed)/sizeof(*fixed)) {
        *type = index < 7 || char_device(fixed[index]->name) ? 2 : fixed[index] == &proc_exe_link ? 10 : 8;
        return fixed[index];
    }
    struct tty_pair *p = &terminals[index - sizeof(fixed)/sizeof(*fixed)];
    *type = 2;
    return p->refs[0] ? &p->node[1] : NULL;
}
static const struct file *maps_snapshot(void);
static long proc_getdents(struct descriptor *d, uintptr_t buf, size_t size, bool tasks) {
    unsigned limit = tasks ? THREAD_LIMIT : MAX_FDS;
    size_t total = 0;
    for (unsigned i = d->cursor->pos; i < limit + 2; i++) {
        struct { uint64_t ino, next; uint16_t reclen; unsigned char type; char name[16]; } rec = {0};
        unsigned len = 0;
        rec.type = 4;
        if (i < 2) {
            rec.name[len++] = '.';
            if (i == 1) rec.name[len++] = '.';
            rec.ino = (uintptr_t)d->file >> 3;
        } else {
            unsigned value = i - 2;
            if (tasks) {
                struct linux_thread *t = &threads[value];
                if (t->state == UNUSED || t->owner != active_process) { d->cursor->pos++; continue; }
                value = t->tid; rec.ino = (uintptr_t)t >> 3;
            } else {
                if (!fds[value].file && !fds[value].pipe) { d->cursor->pos++; continue; }
                rec.type = 10;
                rec.ino = (uintptr_t)&fds[value] >> 3;
            }
            unsigned divisor = 1;
            while (value / divisor >= 10) divisor *= 10;
            for (; divisor; divisor /= 10) rec.name[len++] = '0' + (value / divisor) % 10;
        }
        rec.next = i + 1; rec.reclen = (19 + len + 1 + 7) & ~7;
        if (rec.reclen > size - total) return total ? (long)total : -22;
        if (!transfer(buf + total, &rec, rec.reclen, true)) return total ? (long)total : -14;
        total += rec.reclen; d->cursor->pos++;
    }
    return total;
}

static long open_path(long at, uintptr_t addr, unsigned flags, unsigned mode) {
    char path[512];
    long err = get_path_at(at, addr, path, sizeof(path));
    if (err) return err;
#ifdef VIOLET_TRACE
    say("open "); say(path); say(" flags "); hex(flags); say("\n");
#endif
    if (flags & LL_OPATH) flags &= LL_OPATH | 0x4000 | 0x8000 | 0x80000;
    if (tty_path(path)) {
        const struct file *node = same(path, ptmx_node.name) ? &ptmx_node : NULL;
        for (unsigned i = 0; !node && i < PTY_COUNT; i++)
            if (terminals[i].refs[0] && same(terminals[i].pathname, path)) node = &terminals[i].node[1];
        if (!node) return -2;
        if (flags & 0x4000) return -20;
        if (!land_open(node, path, false, flags)) return -13;
        if (flags & LL_OPATH) {
            int fd = install_regular(node, flags);
            if (fd >= 0) tty_ref(node, 1);
            return fd;
        }
        return tty_open(path, flags);
    }
    const struct file *device = char_device(path);
    if (!device) device = display_node(path);
    if (device) {
        if (flags & 0x4000) return -20;
        if (!land_open(device, path, false, flags)) return -13;
        if ((flags & LL_OPATH) || char_device(path)) return install_regular(device, flags);
        if (device == &audio_node) return open_sound(flags);
        if (device == &sys_node) return open_gui_control(flags);
        if (device == &events_node && (flags & 3)) return -13;
        int fd = free_fd();
        if (fd >= 0) fds[fd] = (struct descriptor){ .file = device, .writer = (flags & 0x800) != 0, .cloexec = !!(flags & 0x80000) };
        return fd;
    }
    struct tmp_node *n = tmp_lookup(path);
    const struct file *f = n ? &n->file : same(path, overcommit.name) ? &overcommit : find_file(path);
    if (!f && same(path, process_status.name)) f = &process_status;
    if (!f && same(path, "/proc/self/maps")) f = maps_snapshot();
    if (!f) f = same(path, task_dir.name) ? &task_dir : packed_dir(path);
    if (f && (flags & 192) == 192) return -17;
    if (!f && (flags & 64)) {
        if ((err = tmp_parent(path))) return err;
        if (!land_parent(path, LL_CREATE) || !land_open(NULL, path, false, flags)) return -13;
        n = tmp_make(path, TMP_REG | (mode & 0777), true);
        if (!n) return -28;
        f = &n->file;
    }
    if (!f) return -2;
    bool dir = land_directory(f);
    if (!land_open(f, path, dir, flags)) return -13;
    if (dir && (flags & 3)) return -21;
    if (!dir && (flags & 0x4000)) return -20;
    if (!n && (flags & 3)) return -30;
    if (n && (flags & 512) && (flags & 3)) tmp_resize(n, 0);
    return install_regular(f, flags);
}

static long make_pipe(uintptr_t result, uint64_t flags) {
    if (flags & ~(0x800UL | 0x80000UL)) return -22;
    struct pipe *p = NULL;
    for (unsigned i = 0; i < sizeof(pipes)/sizeof(*pipes); i++)
        if (!pipes[i].readers && !pipes[i].writers) { p = &pipes[i]; break; }
    if (!p) return -24;
    int ends[2];
    if ((ends[0] = free_fd()) < 0) return ends[0];
    fds[ends[0]] = (struct descriptor){.pipe = p, .cloexec = !!(flags & 0x80000)};
    if ((ends[1] = free_fd()) < 0 || !transfer(result, ends, sizeof(ends), true)) {
        fds[ends[0]] = (struct descriptor){0};
        return ends[1] < 0 ? ends[1] : -14;
    }
    p->nonblock[0] = p->nonblock[1] = !!(flags & 0x800);
    p->count = p->head = 0; p->readers = p->writers = 1;
    fds[ends[1]] = (struct descriptor){.pipe = p, .writer = true, .cloexec = !!(flags & 0x80000)};
    return 0;
}

static const struct file console = {"/dev/console", NULL, 0};
static long stream_io(uint64_t fd, uintptr_t addr, size_t count, bool write) {
    if (fd >= MAX_FDS) return -9;
    if (fds[fd].file == &console) {
        if (fds[fd].writer != write) return -9;
        if (!write) return 0;
        for (size_t i = 0; i < count; i++) {
            unsigned char c;
            if (!transfer(addr + i, &c, 1, false)) return i ? (long)i : -14;
            seL4_DebugPutChar(c);
        }
        return count;
    }
    if (fd >= MAX_FDS) return -9;
    struct descriptor *d = &fds[fd];
    unsigned side;
    struct tty_pair *terminal = tty_of(d->file, &side);
    if (terminal) return tty_stream(terminal, side, d, addr, count, write);
#ifdef VIOLET_NETWORK
    if (d->inet) return inet_io(d->inet, addr, count, write, 0, 0, 0);
#endif
    struct local_socket *local = local_of(d->file);
    if (event_of(d->file)) return event_rw(event_of(d->file), addr, count, write);
    if (local) return local_stream(local, addr, count, write, 0);
    if (d->file == &audio_node) return write ? write_sound(d, addr, count) : -9;
    if (d->file == &sys_node) return gui_io(d, addr, count, write);
    if (d->file == &screen_node) return write ? screen_present(d, addr, count) : -9;
    if (d->file == &events_node) return write ? -9 : read_touch(d, addr, count);
    if (d->file == &urandom_node || d->file == &random_node) {
        if (!d->cursor || (d->cursor->flags & 3) == (write ? 0 : 1)) return -9;
        return write ? -95 : read_entropy(addr, count, d->cursor->flags & 0x800);
    }
    if (d->file == &null_node || d->file == &zero_node) {
        if (write ? !(d->cursor->flags & 3) : (d->cursor->flags & 3) == 1) return -9;
        if (write) return count;
        if (d->file == &null_node) return 0;
        static unsigned char zeroes[PAGE];
        size_t done = 0;
        while (done < count) {
            size_t n = count - done;
            if (n > PAGE) n = PAGE;
            if (!transfer(addr + done, zeroes, n, true)) return done ? (long)done : -14;
            done += n;
        }
        return done;
    }
    if (d->file == &task_dir || is_packed_dir(d->file)) return -21;
    if (tmp_node_of(d->file) && d->cursor) return tmp_rw(d, addr, count, write);
    if (d->file && d->cursor && !write) {
        if (d->cursor->pos >= d->file->size) return 0;
        size_t len = d->file->size - d->cursor->pos;
        if (len > count) len = count;
        if (!transfer(addr, (void *)(d->file->data + d->cursor->pos), len, true)) return -14;
        d->cursor->pos += len;
        return len;
    }
    if (!d->pipe || d->writer != write) return -9;
    struct pipe *p = d->pipe;
    if (write && !p->readers) return -32;
    size_t len = write ? sizeof(p->data) - p->count : p->count;
    if (len > count) len = count;
    if (write && count <= sizeof(p->data) && len < count) len = 0; // PIPE_BUF writes are atomic
    if (!len && count && (write || p->writers))
        return p->nonblock[write] ? -11 : retry_io(ticks_now() + timer_hz/200);
    size_t offset = (p->head + (write ? p->count : 0)) % sizeof(p->data);
    size_t first = sizeof(p->data) - offset;
    if (first > len) first = len;
    if (!transfer(addr, p->data + offset, first, !write) ||
        !transfer(addr + first, p->data, len - first, !write)) return -14;
    if (len && (p->count == 0 || p->count == sizeof(p->data))) wake_pollers(p);
    if (write) p->count += len;
    else { p->head = (p->head + len) % sizeof(p->data); p->count -= len; }
    return len;
}

struct arm_stat {
    uint64_t dev, ino;
    uint32_t mode, nlink, uid, gid;
    uint64_t rdev, pad1;
    int64_t size;
    int32_t block_size, pad2;
    int64_t blocks;
    int64_t times[6];
    uint32_t unused[2];
};
_Static_assert(sizeof(struct arm_stat) == 128, "Linux AArch64 stat layout");

static long stat_to_guest(const struct file *f, bool dir, uintptr_t addr) {
    struct arm_stat st = {0};
    st.dev = 1;
    st.ino = f ? ((uintptr_t)f >> 3) : 2;
    struct tmp_node *n = tmp_node_of(f);
    st.mode = n ? n->mode : (dir || f == &task_dir || is_packed_dir(f)) ? 0040555 : 0100555;
    if (f && char_device(f->name)) {
        st.mode = 0020666; st.rdev = (1 << 8) | (f == &null_node ? 3 : 5);
    }
    st.nlink = 1; st.uid = st.gid = 1000;
    st.size = f ? f->size : 4096; st.block_size = PAGE; st.blocks = (st.size + 511) / 512;
    return transfer(addr, &st, sizeof(st), true) ? 0 : -14;
}
static long stat_fd(uint64_t fd, uintptr_t addr) {
    if (fd >= MAX_FDS) return -9;
    struct descriptor *d = &fds[fd];
    bool socket = d->inet || local_of(d->file);
    if (d->pipe || socket || (d->file && (d->file == &console || display_node(d->file->name)))) {
        struct arm_stat st = {.dev=2, .ino=(uintptr_t)(d->pipe ? (void *)d->pipe : (void *)d->file) >> 3,
                              .nlink=1, .uid=1000, .gid=1000, .block_size=PAGE};
        st.mode = (d->pipe ? 0010000 : socket ? 0140000 : 0020000) | 0600;
        return transfer(addr, &st, sizeof(st), true) ? 0 : -14;
    }
    return d->file ? stat_to_guest(d->file, is_packed_dir(d->file), addr) : -9;
}
static long readlink_guest(int at, uintptr_t name, uintptr_t out, size_t len) {
    char path[512];
    int err = get_path_at(at, name, path, sizeof(path));
    if (err) return err;
    const char *target = NULL;
    if (tmp_lookup(path) || find_file(path) || packed_dir(path)) return -22;
    if (same(path, "/proc/self/exe")) target = active_process->image->name;
    else {
        const char prefix[] = "/proc/self/fd/";
        size_t i = 0;
        while (prefix[i] && prefix[i] == path[i]) i++;
        if (prefix[i]) return -2;
        unsigned fd = 0;
        for (; path[i]; i++) {
            if (path[i] < '0' || path[i] > '9' || fd >= MAX_FDS) return -2;
            fd = fd * 10 + path[i] - '0';
        }
        if (fd < MAX_FDS && fds[fd].file) target = fds[fd].file->name;
    }
    if (!target) return -2;
    size_t n = 0;
    while (n < len && target[n]) n++;
    return transfer(out, (void *)target, n, true) ? (long)n : -14;
}

static char map_storage[PROCESS_LIMIT][128 * 1024];
static struct file proc_maps[PROCESS_LIMIT];
static char *number_hex(char *pos, uint64_t n) {
    unsigned bits = 60;
    while (bits && !(n >> bits)) bits -= 4;
    for (;;) {
        *pos++ = "0123456789abcdef"[n >> bits & 15];
        if (!bits) return pos;
        bits -= 4;
    }
}
static const struct file *maps_snapshot(void) {
    uintptr_t last = 0;
    unsigned index = active_process - processes;
    char *map_text = map_storage[index], *p = map_text;
    for (unsigned j = 0; j < nregions; j++) {
        const struct region *r = NULL;
        for (unsigned i = 0; i < nregions; i++)
            if (regions[i].lo >= last && (!r || r->lo > regions[i].lo)) r = &regions[i];
        if (!r || (size_t)(p - map_text) + 128 >= sizeof(map_storage[index])) break;
        last = r->hi;
        p = number_hex(p, r->lo); *p++ = '-'; p = number_hex(p, r->hi); *p++ = ' ';
        *p++ = r->prot & 1 ? 'r' : '-'; *p++ = r->prot & 2 ? 'w' : '-';
        *p++ = r->prot & 4 ? 'x' : '-';
        const char *suffix = r->hi == STACK_TOP ? "p 00000000 00:00 0 [stack]\n" : "p 00000000 00:00 0\n";
        while (*suffix) *p++ = *suffix++;
    }
    proc_maps[index] = (struct file){"/proc/self/maps", (unsigned char *)map_text, p - map_text};
    return &proc_maps[index];
}
