static long mkdir_guest(int at, uintptr_t name, unsigned mode) {
    char path[512];
    int err = get_path_at(at, name, path, sizeof(path));
    if (err)
        return err;
    if (tmp_lookup(path) || find_file(path) || packed_dir(path))
        return -17;
    if ((err = tmp_parent(path)))
        return err;
    if (!land_parent(path, LL_MKDIR)) return -13;
    return tmp_make(path, TMP_DIR | (mode & 0777), true) ? 0 : -28;
}
static long install_regular(const struct file *f, unsigned flags) {
    int fd = free_fd();
    if (fd < 0)
        return fd;
    for (unsigned i = 0; i < MAX_FDS; i++)
        if (!cursors[i].refs) {
            cursors[i] = (struct file_cursor){.refs = 1, .flags = flags & ~0x80000u};
            fds[fd] = (struct descriptor){.file = f, .cursor = &cursors[i], .cloexec = !!(flags & 0x80000)};
            struct tmp_node *n = tmp_node_of(f);
            if (n)
                ++n->opens;
            return fd;
        }
    return -23;
}
static long tmp_resize(struct tmp_node *n, uint64_t size) {
    if (!n)
        return -30;
    if ((n->mode & 0170000) != TMP_REG || size > INT64_MAX)
        return -22;
    if (size < n->file.size && (n->seals & 2))
        return -1;
    if (size > n->file.size && (n->seals & 4))
        return -1;
    if (size < n->file.size) {
        // Revoke guest mappings before making their backing frames available to other files.
        struct vm_space *previous = vm;
        for (unsigned i = 0; i < PROCESS_LIMIT + 1; i++) {
            if (!spaces[i].users) continue;
            vm = &spaces[i];
            for (unsigned j = 0; j < nregions; j++) {
                struct region *r = &regions[j];
                if (r->tmp != n) continue;
                uint64_t file_offset = r->tmp_offset + (r->lo - r->tmp_base);
                uint64_t retained = up(size) > file_offset ? up(size) - file_offset : 0;
                if (retained < r->hi - r->lo) update_pages(r->lo + retained, r->hi, -1);
            }
        }
        vm = previous;
        unsigned *link = &n->first;
        while (*link) {
            unsigned id = *link;
            struct tmp_block *b = &tmp_blocks[id - 1];
            if (b->offset >= size) {
                unsigned *bucket = &n->buckets[b->offset / PAGE % TMP_BUCKETS];
                while (*bucket != id) bucket = &tmp_blocks[*bucket - 1].bucket_next;
                *bucket = b->bucket_next;
                *link = b->next;
                release_tmp_block(id);
            } else {
                if (size - b->offset < PAGE) {
                    size_t tail = size - b->offset;
                    memset(tmp_bytes(b) + tail, 0, PAGE - tail);
                }
                link = &b->next;
            }
        }
    }
    n->file.size = size;
    return 0;
}

static long guest_statfs(const struct file *file, uintptr_t dest) {
    // AArch64 Linux statfs fields are 64 bits, including the two-part fsid.
    uint64_t stat[15] = {0};
    stat[1] = stat[9] = PAGE;
    stat[8] = 255;
    if (tmp_node_of(file)) {
        stat[0] = 0x01021994; // TMPFS_MAGIC
        stat[2] = TMP_BLOCKS;
        stat[3] = stat[4] = TMP_BLOCKS - used_blocks;
        stat[5] = TMP_NODES;
        for (unsigned i = 0; i < TMP_NODES; i++) if (!tmp_nodes[i].mode) ++stat[6];
    } else {
        stat[0] = 0x7275; // ROMFS_MAGIC for the immutable file bundle
        for (const struct file *f = fs_begin; f < fs_end; f++) stat[2] += up(f->size) / PAGE;
        stat[5] = (fs_end - fs_begin) + (fs_dirs_end - fs_dirs);
        stat[10] = 1; // ST_RDONLY
    }
    stat[7] = stat[0];
    stat[10] |= 32; // ST_VALID
    return transfer(dest, stat, sizeof(stat), true) ? 0 : -14;
}
static long tmp_rw(struct descriptor *d, uintptr_t addr, size_t count, bool write) {
    struct tmp_node *n = tmp_node_of(d->file);
    unsigned access = d->cursor->flags & 3;
    if ((n->mode & 0170000) == TMP_DIR)
        return -21;
    if (write ? access == 0 : access == 1)
        return -9;
    size_t pos = write && (d->cursor->flags & 1024) ? n->file.size : d->cursor->pos;
    if (count > INT64_MAX - pos)
        return -27;
    if (write && ((n->seals & 24) || ((n->seals & 4) && pos + count > n->file.size)))
        return -1;
    if (!write) {
        if (pos >= n->file.size)
            return 0;
        if (count > n->file.size - pos)
            count = n->file.size - pos;
    }
    static unsigned char zeros[PAGE];
    size_t done = 0;
    while (done < count) {
        size_t within = (pos + done) % PAGE, len = PAGE - within;
        if (len > count - done)
            len = count - done;
        struct tmp_block *b = tmp_page(n, pos + done, write);
        if (write && !b)
            return done ? (long)done : -28;
        if (!transfer(addr + done, (b ? tmp_bytes(b) : zeros) + within, len, !write))
            return done ? (long)done : -14;
        done += len;
        d->cursor->pos = pos + done;
        if (write && pos + done > n->file.size)
            n->file.size = pos + done;
    }
    return done;
}
static long memfd_guest(uintptr_t name, unsigned flags) {
    if (flags & ~3u)
        return -22;
    char path[300] = "/memfd:";
    for (unsigned i = 7;; i++) {
        if (i >= 257)
            return -22;
        if (!transfer(name++, path + i, 1, false))
            return -14;
        if (!path[i])
            break;
    }
    struct tmp_node *n = tmp_make(path, TMP_REG | 0700, false);
    if (!n)
        return -28;
    n->memfd = true;
    n->seals = (flags & 2) ? 0 : 1;
    return install_regular(&n->file, 2 | ((flags & 1) ? 0x80000 : 0));
}
static const char *child_name(const char *parent, const char *path) {
    unsigned i = 0;
    while (parent[i] && parent[i] == path[i])
        i++;
    if (parent[i])
        return NULL;
    if (i > 1 && path[i++] != '/')
        return NULL;
    return path[i] ? path + i : NULL;
}
static long unlink_guest(int at, uintptr_t addr, unsigned flags) {
    if (flags & ~512u)
        return -22;
    char path[512];
    int err = get_path_at(at, addr, path, sizeof(path));
    if (err)
        return err;
    struct tmp_node *n = tmp_lookup(path);
    if (!n)
        return find_file(path) || packed_dir(path) ? -30 : -2;
    if (same(path, "/tmp") || !tmp_path(path))
        return -30;
    bool dir = (n->mode & 0170000) == TMP_DIR;
    if (!land_parent(path, dir ? LL_RMDIR : LL_UNLINK)) return -13;
    if (dir != !!flags)
        return dir ? -21 : -20;
    if (dir)
        for (unsigned i = 0; i < TMP_NODES; i++)
            if (tmp_nodes[i].linked && child_name(path, tmp_nodes[i].path))
                return -39;
    n->linked = false;
    tmp_collect(n);
    return 0;
}
// Only mutable files are stored in this table. Packed directories have their own records.
static long tmp_getdents(unsigned fd, uintptr_t addr, size_t max) {
    struct tmp_node *dir = tmp_node_of(fds[fd].file);
    if (!dir || (dir->mode & 0170000) != TMP_DIR)
        return -20;
    size_t written = 0;
    struct file_cursor *c = fds[fd].cursor;
    while (c->pos < 2 + TMP_NODES) {
        size_t index = c->pos;
        struct tmp_node *entry = dir;
        const char *name = index == 0 ? "." : "..";
        if (index >= 2) {
            entry = &tmp_nodes[index - 2];
            name = entry->linked ? direct_child(dir->path, entry->path) : NULL;
            if (!name) {
                c->pos++;
                continue;
            }
        }
        size_t n = path_length(name);
        uint16_t size = (19 + n + 1 + 7) & ~7u;
        if (size > max - written)
            return written ? (long)written : -22;
        unsigned char record[544] = {0};
        uint64_t ino = (uintptr_t)entry >> 3, off = index + 1;
        memcpy(record, &ino, 8);
        memcpy(record + 8, &off, 8);
        memcpy(record + 16, &size, 2);
        record[18] = entry->mode >> 12;
        memcpy(record + 19, name, n);
        if (!transfer(addr + written, record, size, true))
            return written ? (long)written : -14;
        written += size;
        c->pos++;
    }
    return written;
}

static long memfd_seal(struct descriptor *d, unsigned cmd, unsigned seals) {
    struct tmp_node *n = tmp_node_of(d->file);
    if (!n || !n->memfd)
        return -22;
    if (cmd == 1034)
        return n->seals;
    if (seals & ~31u)
        return -22;
    if (n->seals & 1)
        return -1;
    if (seals & 8)
        for (unsigned i = 0; i < PROCESS_LIMIT + 1; i++)
            if (spaces[i].users)
                for (unsigned j = 0; j < spaces[i].count; j++) {
                    struct region *r = &spaces[i].areas[j];
                    if (r->tmp == n && r->tmp_shared && !r->shared_readonly)
                        return -16;
                }
    n->seals |= seals;
    return 0;
}

static long rename_guest(seL4_UserContext *r, unsigned flags) {
    if (flags & ~1u) return -22;
    char from[512], to[512];
    long err = get_path_at(r->x0, r->x1, from, sizeof(from));
    if (!err) err = get_path_at(r->x2, r->x3, to, sizeof(to));
    if (err) return err;
    struct tmp_node *source = tmp_lookup(from), *dest = tmp_lookup(to);
    if (!source) return find_file(from) || packed_dir(from) ? -30 : -2;
    if (!tmp_path(from) || same(from, "/tmp")) return -30;
    if ((err = tmp_parent(to))) return err;
    if (caller->domain.count) {
        char parent_from[512], parent_to[512]; parent_name(from, parent_from); parent_name(to, parent_to);
        bool directory = (source->mode & 0170000) == TMP_DIR;
        if (!land_parent(from, directory ? LL_RMDIR : LL_UNLINK) ||
            !land_parent(to, directory ? LL_MKDIR : (source->mode & 0170000) == TMP_SOCK ? LL_BIND : LL_CREATE) ||
            (dest && !land_parent(to, (dest->mode & 0170000) == TMP_DIR ? LL_RMDIR : LL_UNLINK))) return -13;
        if (!same(parent_from, parent_to)) return -18; // ABI 1 always denies reparenting
    }
    if (source == dest) return (flags & 1) ? -17 : 0;
    bool dir = (source->mode & 0170000) == TMP_DIR;
    if (dest) {
        if (flags & 1) return -17;
        if (dir != ((dest->mode & 0170000) == TMP_DIR)) return dir ? -20 : -21;
        for (unsigned i = 0; dir && i < TMP_NODES; i++)
            if (tmp_nodes[i].linked && child_name(to, tmp_nodes[i].path)) return -39;
    }
    if (dir && child_name(from, to)) return -22;
    size_t prefix = path_length(to);
    if (dir) for (unsigned i = 0; i < TMP_NODES; i++) if (tmp_nodes[i].linked) {
        const char *suffix = child_name(from, tmp_nodes[i].path);
        if (suffix && prefix + path_length(suffix) + 2 > sizeof(to)) return -36;
    }
    if (dest) { dest->linked = false; tmp_collect(dest); }
    if (dir) for (unsigned i = 0; i < TMP_NODES; i++) if (tmp_nodes[i].linked) {
        const char *suffix = child_name(from, tmp_nodes[i].path);
        if (!suffix) continue;
        char new_name[512];
        memcpy(new_name, to, prefix); new_name[prefix] = '/';
        size_t rest = path_length(suffix) + 1;
        memcpy(new_name + prefix + 1, suffix, rest);
        memcpy(tmp_nodes[i].path, new_name, prefix + 1 + rest);
    }
    memcpy(source->path, to, prefix + 1); return 0;
}
