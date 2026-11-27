// Landlock filesystem ABI 1. A mutable ruleset fd builds policies. Each
// restriction takes an immutable snapshot, inherited by newly created threads.
enum {
    LL_EXEC = 1,
    LL_WRITE = 2,
    LL_READ = 4,
    LL_LIST = 8,
    LL_RMDIR = 16,
    LL_UNLINK = 32,
    LL_MKDIR = 128,
    LL_CREATE = 256,
    LL_BIND = 512,
    LL_FS_MASK = 8191,
    LL_OPATH = 0x200000,
    LL_RULES = 256,
    LL_POLICIES = 128
};
struct land_rule {
    const struct file *file;
    unsigned rights;
    bool directory;
};
struct land_policy {
    struct file file;
    unsigned handled, used, refs;
    bool snapshot;
    struct land_rule rules[LL_RULES];
};
static struct land_policy land_policies[LL_POLICIES];
static struct land_policy *land_fd(const struct file *file) {
    return (uintptr_t)file >= (uintptr_t)land_policies &&
                   (uintptr_t)file < (uintptr_t)(land_policies + LL_POLICIES)
               ? (struct land_policy *)file
               : NULL;
}
static void land_pin(const struct land_rule *rule, bool add) {
    tty_ref(rule->file, add ? 1 : -1);
    struct tmp_node *n = tmp_node_of(rule->file);
    if (!n)
        return;
    if (add)
        n->opens++;
    else {
        --n->opens;
        tmp_collect(n);
    }
}
static void land_collect(struct land_policy *policy) {
    if (policy->refs)
        return;
    if (policy->snapshot) {
        for (unsigned i = 0; i < THREAD_LIMIT; i++) {
            struct linux_thread *t = &threads[i];
            if (t->state == UNUSED)
                continue;
            for (unsigned j = 0; j < t->domain.count; j++)
                if (t->domain.layers[j] == policy)
                    return;
        }
    }
    for (unsigned i = 0; i < policy->used; i++)
        land_pin(&policy->rules[i], false);
    policy->handled = 0;
}
static struct land_policy *land_alloc(unsigned handled, bool snapshot) {
    for (unsigned i = 0; i < LL_POLICIES; i++) {
        struct land_policy *p = &land_policies[i];
        if (p->handled)
            land_collect(p);
        if (p->handled)
            continue;
        p->file.name = "anon_inode:[landlock-ruleset]";
        p->handled = handled;
        p->snapshot = snapshot;
        p->refs = !snapshot;
        p->used = 0;
        return p;
    }
    return NULL;
}
static long land_create(uintptr_t attr, size_t size, unsigned flags) {
    if (flags == 1)
        return !attr && !size ? 1 : -22;
    if (flags)
        return -22;
    if (!attr)
        return -14;
    if (size < 8)
        return -22;
    if (size > PAGE)
        return -7;
    uint64_t handled;
    if (!transfer(attr, &handled, sizeof(handled), false))
        return -14;
    for (size_t i = 8; i < size; i++) {
        unsigned char byte;
        if (!transfer(attr + i, &byte, 1, false))
            return -14;
        if (byte)
            return -7;
    }
    if (handled & ~(uint64_t)LL_FS_MASK)
        return -22;
    if (!handled)
        return -42;
    int fd = free_fd();
    if (fd < 0)
        return fd;
    struct land_policy *p = land_alloc(handled, false);
    if (!p)
        return -12;
    fds[fd] = (struct descriptor){.file = &p->file, .cloexec = true};
    return fd;
}
static bool land_directory(const struct file *f);
static long land_add(unsigned fd, unsigned type, uintptr_t attr, unsigned flags) {
    if (flags || type != 1)
        return -22;
    if (fd >= MAX_FDS || (!fds[fd].file && !fds[fd].pipe))
        return -9;
    struct land_policy *p = land_fd(fds[fd].file);
    if (!p)
        return -77;
    struct {
        uint64_t allowed;
        int32_t parent;
    } rule;
    if (!transfer(attr, &rule, 12, false))
        return -14;
    if (!rule.allowed)
        return -42;
    if (rule.allowed & ~(uint64_t)p->handled)
        return -22;
    if ((unsigned)rule.parent >= MAX_FDS || !fds[rule.parent].file)
        return -9;
    const struct file *f = fds[rule.parent].file;
    if (same(f->name, ptmx_node.name)) f = &ptmx_node;
    struct tmp_node *n = tmp_node_of(f);
    if (f->name[0] != '/' || (n && n->memfd))
        return -77;
    bool directory = land_directory(f);
    if (!directory && (rule.allowed & ~(uint64_t)(LL_EXEC | LL_WRITE | LL_READ)))
        return -22;
    for (unsigned i = 0; i < p->used; i++)
        if (p->rules[i].file == f) {
            p->rules[i].rights |= rule.allowed;
            return 0;
        }
    if (p->used == LL_RULES)
        return -12;
    struct land_rule *r = &p->rules[p->used++];
    *r = (struct land_rule){f, rule.allowed, directory};
    land_pin(r, true);
    return 0;
}
static long land_restrict(unsigned fd, unsigned flags) {
    if (flags)
        return -22;
    if (!caller->no_new_privs)
        return -1;
    if (fd >= MAX_FDS || (!fds[fd].file && !fds[fd].pipe))
        return -9;
    struct land_policy *source = land_fd(fds[fd].file);
    if (!source)
        return -77;
    if (caller->domain.count == 16)
        return -7;
    struct land_policy *p = land_alloc(source->handled, true);
    if (!p)
        return -12;
    p->used = source->used;
    memcpy(p->rules, source->rules, p->used * sizeof(*p->rules));
    for (unsigned i = 0; i < p->used; i++)
        land_pin(&p->rules[i], true);
    caller->domain.layers[caller->domain.count++] = p;
    return 0;
}
// A rule keeps the inode alive. A new object that reuses an unlinked name
// is distinct; moving a still-linked subtree keeps its inode-based grant.
static bool land_beneath(const struct land_rule *rule, const struct file *file, const char *path) {
    if (file && rule->file == file)
        return true;
    if (!rule->directory)
        return false;
    struct tmp_node *n = tmp_node_of(rule->file);
    if (n && !n->linked)
        return false;
    const char *dir = rule->file->name;
    while (*dir && *dir == *path) {
        dir++;
        path++;
    }
    return !*dir && (!*path || *path == '/' || dir[-1] == '/');
}
static bool land_allowed(const struct file *file, const char *path, unsigned rights) {
    if (!caller)
        return true;
    for (unsigned i = 0; i < caller->domain.count; i++) {
        struct land_policy *p = caller->domain.layers[i];
        unsigned need = p->handled & rights;
        for (unsigned j = 0; need && j < p->used; j++)
            if (land_beneath(&p->rules[j], file, path))
                need &= ~p->rules[j].rights;
        if (need)
            return false;
    }
    return true;
}
static bool land_exec(const struct file *file) { return land_allowed(file, file->name, LL_EXEC); }
static bool land_open(const struct file *file, const char *path, bool dir, unsigned flags) {
    if (flags & LL_OPATH)
        return true;
    unsigned rights = (flags & 3) == 1 ? 0 : dir ? LL_LIST : LL_READ;
    if (flags & 3)
        rights |= LL_WRITE;
    return land_allowed(file, path, rights);
}
static void parent_name(const char *path, char *parent) {
    size_t n = path_length(path);
    while (n > 1 && path[n - 1] != '/')
        n--;
    if (n > 1)
        n--;
    memcpy(parent, path, n);
    parent[n] = 0;
}
static bool land_parent(const char *path, unsigned rights) {
    char parent[512];
    parent_name(path, parent);
    struct tmp_node *n = tmp_lookup(parent);
    return land_allowed(n ? &n->file : packed_dir(parent), parent, rights);
}
static bool path_only(const struct descriptor *d) { return d->cursor && (d->cursor->flags & LL_OPATH); }
