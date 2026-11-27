// Packed, read-only filesystem supplied by the QEMU launcher.
struct file { const char *name; const unsigned char *data; size_t size; };
extern const struct file fs_begin[], fs_end[], fs_dirs[], fs_dirs_end[];
struct pipe { unsigned char data[4096]; size_t head, count; int readers, writers; bool nonblock[2]; };
static struct pipe pipes[512];
enum { MAX_FDS = 65536 };
struct file_cursor { unsigned refs, flags; size_t pos; };
static struct file_cursor cursors[MAX_FDS];
struct descriptor {
    const struct file *file;
    struct file_cursor *cursor;
    uintptr_t pos; // virtual device mapping in this process
    struct inet_socket *inet;
    struct pipe *pipe;
    bool writer, cloexec;
};
struct fd_table { struct descriptor entries[MAX_FDS]; unsigned soft, hard; };
static struct fd_table *file_table;
#define fds (file_table->entries)
#define fd_limit (file_table->soft)
#define fd_hard_limit (file_table->hard)
static int free_fd(void) {
    for (unsigned i = 0; i < fd_limit; i++) if (!fds[i].file && !fds[i].pipe) return i;
    return -24;
}

static bool same(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return *a == *b;
}
static const struct file *find_file(const char *path) {
    for (const struct file *f = fs_begin; f < fs_end; f++)
        if (same(f->name, path)) return f;
    return NULL;
}
static void canonicalize(char *s) {
    char *r = s, *w = s;
    while (*r) {
        while (*r == '/') r++;
        char *component = r;
        while (*r && *r != '/') r++;
        size_t n = r - component;
        if (!n || (n == 1 && *component == '.')) continue;
        if (n == 2 && component[0] == '.' && component[1] == '.') {
            while (w != s && w[-1] != '/') w--;
            if (w != s) w--;
            continue;
        }
        if (*r) r++;
        *w++ = '/';
        while (n--) *w++ = *component++;
    }
    if (w == s) *w++ = '/';
    *w = 0;
}
