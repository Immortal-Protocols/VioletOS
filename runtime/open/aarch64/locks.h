// POSIX byte-range locks belong to the process and inode. Closing any
// descriptor for that inode releases this process's locks, including on exit.
enum { FILE_LOCK_LIMIT = 256, READ_LOCK = 0, WRITE_LOCK = 1, UNLOCK = 2 };
struct record_lock {
    const struct file *file;
    uint64_t start, end; // exclusive end, UINT64_MAX means through EOF
    int pid, type;
};
struct linux_flock { int16_t type, whence; int32_t pad; int64_t start, len; int32_t pid, tail; };
_Static_assert(sizeof(struct linux_flock) == 32, "AArch64 flock layout");
static struct record_lock record_locks[FILE_LOCK_LIMIT], lock_work[FILE_LOCK_LIMIT];
static unsigned lock_count;
static void release_file_locks(const struct file *file, int pid) {
    for (unsigned i = 0; i < lock_count;)
        if (record_locks[i].file == file && record_locks[i].pid == pid)
            record_locks[i] = record_locks[--lock_count];
        else i++;
}
static bool save_lock(unsigned *count, struct record_lock lock) {
    if (*count == FILE_LOCK_LIMIT) return false;
    lock_work[(*count)++] = lock;
    return true;
}
static long file_lock(struct descriptor *d, unsigned op, uintptr_t ptr) {
    if (!d->cursor) return -9;
    struct linux_flock flock;
    if (!transfer(ptr, &flock, sizeof(flock), false)) return -14;
    if (flock.whence < 0 || flock.whence > 2 || flock.type < READ_LOCK || flock.type > UNLOCK) return -22;
    unsigned mode = d->cursor->flags & 3;
    if (op != 5 && ((flock.type == WRITE_LOCK && !mode) || (flock.type == READ_LOCK && mode == 1))) return -9;
    int64_t start = flock.whence == 1 ? d->cursor->pos : flock.whence == 2 ? d->file->size : 0;
    if (__builtin_add_overflow(start, flock.start, &start)) return -75;
    int64_t bound;
    if (__builtin_add_overflow(start, flock.len, &bound)) return -75;
    if (start < 0 || bound < 0) return -22;
    if (flock.len < 0) { int64_t old = start; start = bound; bound = old; }
    struct record_lock req = {d->file, (uint64_t)start, flock.len ? (uint64_t)bound : UINT64_MAX,
                              active_process->pid, flock.type};
    if (op == 5 && req.type == UNLOCK) return -22;
    if (req.type != UNLOCK) {
        for (unsigned i = 0; i < lock_count; i++) {
            struct record_lock lock = record_locks[i];
            if (lock.file != req.file || lock.pid == req.pid || lock.end <= req.start || req.end <= lock.start ||
                (req.type == READ_LOCK && lock.type == READ_LOCK)) continue;
            if (op != 5) return op == 7 ? retry_io(ticks_now() + timer_hz/200) : -11;
            flock.type = lock.type; flock.whence = 0; flock.start = lock.start;
            flock.len = lock.end == UINT64_MAX ? 0 : (int64_t)(lock.end - lock.start);
            flock.pid = lock.pid;
            return transfer(ptr, &flock, sizeof(flock), true) ? 0 : -14;
        }
    }
    if (op == 5) {
        flock.type = UNLOCK;
        return transfer(ptr, &flock, sizeof(flock), true) ? 0 : -14;
    }
    // Stage replacements so ENOLCK leaves the original lock set intact.
    unsigned count = 0;
    for (unsigned i = 0; i < lock_count; i++) {
        struct record_lock lock = record_locks[i];
        if (lock.file != req.file || lock.pid != req.pid || lock.end <= req.start || req.end <= lock.start) {
            if (!save_lock(&count, lock)) return -37;
            continue;
        }
        struct record_lock left = lock; left.end = req.start;
        if (left.start < left.end && !save_lock(&count, left)) return -37;
        lock.start = req.end;
        if (lock.start < lock.end && !save_lock(&count, lock)) return -37;
    }
    if (req.type != UNLOCK) {
        for (unsigned i = 0; i < count;) {
            struct record_lock lock = lock_work[i];
            if (lock.file == req.file && lock.pid == req.pid && lock.type == req.type &&
                (lock.end == req.start || req.end == lock.start)) {
                if (lock.start < req.start) req.start = lock.start;
                if (lock.end > req.end) req.end = lock.end;
                lock_work[i] = lock_work[--count];
            } else i++;
        }
        if (!save_lock(&count, req)) return -37;
    }
    memcpy(record_locks, lock_work, count * sizeof(*lock_work));
    lock_count = count;
    return 0;
}
