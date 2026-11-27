// Directory entries refer to the same immutable namespace used for loading
// programs and shared libraries. Offsets are stable across getdents64 calls.
static bool is_packed_dir(const struct file *f) {
    return (uintptr_t)f >= (uintptr_t)fs_dirs && (uintptr_t)f < (uintptr_t)fs_dirs_end;
}
static const struct file *packed_dir(const char *path) {
    for (const struct file *d = fs_dirs; d < fs_dirs_end; d++)
        if (same(d->name, path)) return d;
    return NULL;
}
static int get_path_at(int at, uintptr_t addr, char *path, size_t size) {
    char first;
    if (!transfer(addr, &first, 1, false)) return -14;
    if (!first) return -2;
    size_t used = 0;
    if (first != '/' && at != -100) {
        if ((unsigned)at >= MAX_FDS || !fds[at].file) return -9;
        struct tmp_node *node = tmp_node_of(fds[at].file);
        if (node && !node->linked) return -2;
        if (fds[at].file != &task_dir && !is_packed_dir(fds[at].file) && (!node || (node->mode & 0170000) != TMP_DIR)) return -20;
        const char *dir = fds[at].file->name;
        while (*dir && used < size) path[used++] = *dir++;
    }
    if (used < size) path[used++] = '/';
    while (used < size) {
        if (!transfer(addr++, path + used, 1, false)) return -14;
        if (!path[used++]) { canonicalize(path); return 0; }
    }
    return -36;
}
static int get_path(uintptr_t addr, char *path, size_t size) {
    return get_path_at(-100, addr, path, size);
}
static const char *direct_child(const char *parent, const char *path) {
    while (*parent && *parent == *path) { parent++; path++; }
    if (*parent) return NULL;
    if (parent[-1] != '/') {
        if (*path != '/') return NULL;
        path++;
    }
    if (!*path) return NULL;
    for (const char *p = path; *p; p++) if (*p == '/') return NULL;
    return path;
}
static const struct file *packed_parent(const struct file *dir) {
    if (same(dir->name, "/")) return dir;
    for (const struct file *d = fs_dirs; d < fs_dirs_end; d++)
        if (direct_child(d->name, dir->name)) return d;
    return dir;
}
static const struct file *runtime_dirent(size_t index, unsigned *type);
enum { RUNTIME_DIRENTS = 17 }; // fixed entries followed by four allocated pseudo-terminals
static long packed_getdents(unsigned fd, uintptr_t buf, size_t len) {
    if (fd >= MAX_FDS || !fds[fd].file) return -9;
    struct descriptor *d = &fds[fd];
    if (!is_packed_dir(d->file)) return -20;
    size_t total = 0, dirs = fs_dirs_end - fs_dirs, files = fs_end - fs_begin;
    for (size_t i = d->cursor->pos; i < 2 + dirs + files + RUNTIME_DIRENTS; i++) {
        const struct file *entry;
        const char *name;
        unsigned type = 4;
        if (i < 2) {
            entry = i ? packed_parent(d->file) : d->file;
            name = i ? ".." : ".";
        } else if (i >= 2 + dirs + files) {
            entry = runtime_dirent(i - 2 - dirs - files, &type);
            name = entry ? direct_child(d->file->name, entry->name) : NULL;
        } else {
            entry = i < 2 + dirs ? &fs_dirs[i - 2] : &fs_begin[i - 2 - dirs];
            name = direct_child(d->file->name, entry->name);
            type = is_packed_dir(entry) ? 4 : 8;
        }
        if (name) {
            struct { uint64_t ino, next; uint16_t reclen; uint8_t type; char name[256]; } record = {0};
            size_t n = 0;
            while (name[n] && n < sizeof(record.name)) n++;
            if (n == sizeof(record.name)) return total ? (long)total : -36;
            memcpy(record.name, name, n);
            record.ino = (uintptr_t)entry >> 3; record.next = i + 1;
            record.type = type;
            record.reclen = (19 + n + 1 + 7) & ~7;
            if (record.reclen > len - total) return total ? (long)total : -22;
            if (!transfer(buf + total, &record, record.reclen, true)) return total ? (long)total : -14;
            total += record.reclen;
        }
        d->cursor->pos = i + 1;
    }
    return total;
}
