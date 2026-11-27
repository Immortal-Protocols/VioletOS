// The bundle stays immutable. /tmp is a writable, sparse in-memory filesystem.
// Every resident file page has one permanent root mapping. An independent
// copied capability maps that same frame into each MAP_SHARED guest region.
#define TMP_ALIAS 0x1000000000ULL
enum { TMP_NODES = 512, TMP_BLOCKS = 32768, TMP_BUCKETS = 64, TMP_DIR = 0040000, TMP_REG = 0100000, TMP_SOCK = 0140000 };
struct tmp_node {
    struct file file;
    char path[512];
    unsigned mode, opens, first, seals;
    unsigned buckets[TMP_BUCKETS];
    bool linked, memfd;
};
struct tmp_block {
    seL4_CPtr cap;
    uint64_t offset;
    unsigned next, bucket_next;
};
static struct tmp_node tmp_nodes[TMP_NODES];
static struct tmp_block tmp_blocks[TMP_BLOCKS];
static unsigned free_blocks, allocated_blocks, used_blocks;
static size_t path_length(const char *s) {
    size_t n = 0;
    while (s[n])
        n++;
    return n;
}
static struct tmp_node *tmp_node_of(const struct file *f) {
    uintptr_t p = (uintptr_t)f;
    if (p < (uintptr_t)tmp_nodes || p >= (uintptr_t)(tmp_nodes + TMP_NODES))
        return NULL;
    return (struct tmp_node *)f; // file is the first member
}
static struct tmp_node *tmp_lookup(const char *name) {
    for (unsigned i = 0; i < TMP_NODES; i++)
        if (tmp_nodes[i].linked && same(tmp_nodes[i].path, name))
            return &tmp_nodes[i];
    return NULL;
}
static bool tmp_path(const char *p) {
    const char *prefix = "/tmp";
    while (*prefix && *p == *prefix) {
        p++;
        prefix++;
    }
    return !*prefix && (!*p || *p == '/');
}
static unsigned char *tmp_bytes(const struct tmp_block *b) {
    return (unsigned char *)(TMP_ALIAS + (b - tmp_blocks) * PAGE);
}
static struct tmp_block *tmp_page(struct tmp_node *n, uint64_t offset, bool create) {
    offset &= -(uint64_t)PAGE;
    unsigned bucket = offset / PAGE % TMP_BUCKETS;
    for (unsigned id = n->buckets[bucket]; id; id = tmp_blocks[id - 1].bucket_next)
        if (tmp_blocks[id - 1].offset == offset)
            return &tmp_blocks[id - 1];
    if (!create)
        return NULL;
    unsigned id = free_blocks;
    if (id) free_blocks = tmp_blocks[id - 1].next;
    else if (allocated_blocks < TMP_BLOCKS) id = ++allocated_blocks;
    else return NULL;
    struct tmp_block *b = &tmp_blocks[id - 1];
    if (!b->cap) {
        b->cap = alloc_object(seL4_ARM_SmallPageObject, 0);
        map(b->cap, seL4_CapInitThreadVSpace, (uintptr_t)tmp_bytes(b), 3);
    }
    memset(tmp_bytes(b), 0, PAGE);
    b->offset = offset;
    b->bucket_next = n->buckets[bucket];
    n->buckets[bucket] = id;
    b->next = n->first;
    n->first = id;
    ++used_blocks;
    return b;
}
static void release_tmp_block(unsigned id) {
    tmp_blocks[id - 1].next = free_blocks;
    free_blocks = id;
    --used_blocks;
}
static bool tmp_mapped(struct tmp_node *n);
static void tmp_collect(struct tmp_node *n) {
    if (n->linked || n->opens || tmp_mapped(n))
        return;
    while (n->first) {
        unsigned id = n->first;
        n->first = tmp_blocks[id - 1].next;
        release_tmp_block(id);
    }
    *n = (struct tmp_node){0};
}
static struct tmp_node *tmp_make(const char *path, unsigned mode, bool linked) {
    for (unsigned i = 0; i < TMP_NODES; i++) {
        struct tmp_node *n = &tmp_nodes[i];
        if (n->mode)
            tmp_collect(n);
        if (n->mode)
            continue;
        memcpy(n->path, path, path_length(path) + 1);
        n->file.name = n->path;
        n->mode = mode;
        n->linked = linked;
        return n;
    }
    return NULL;
}
static int tmp_parent(const char *path) {
    if (!tmp_path(path) || same(path, "/tmp"))
        return -30;
    char parent[512];
    memcpy(parent, path, path_length(path) + 1);
    size_t end = path_length(parent);
    while (end && parent[end - 1] != '/')
        end--;
    parent[end - 1] = 0;
    struct tmp_node *d = tmp_lookup(parent);
    return !d ? -2 : (d->mode & 0170000) == TMP_DIR ? 0 : -20;
}
