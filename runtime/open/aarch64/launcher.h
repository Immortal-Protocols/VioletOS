// Home uses the same line-oriented /dev/violet/sys open/home/running verbs as ARMv7.
static int launched_pid[PROCESS_LIMIT];
static const struct file sys_node = {"/dev/violet/sys", NULL, 0};
struct gui_command {
    struct file_cursor cursor; // reference counted across dup and fork
    unsigned used;
    char line[256], answer[512];
};
static struct gui_command commands[32];

static long open_gui_control(unsigned flags) {
    int fd = free_fd();
    if (fd < 0) return fd;
    for (unsigned i = 0; i < sizeof(commands) / sizeof(commands[0]); i++) {
        struct gui_command *c = &commands[i];
        if (c->cursor.refs) continue;
        memset(c, 0, sizeof(*c));
        c->cursor.refs = 1;
        fds[fd] = (struct descriptor){.file=&sys_node, .cursor=&c->cursor, .cloexec=!!(flags & 0x80000)};
        return fd;
    }
    return -23;
}

static long launch_app(const struct file *file) {
    struct process *parent = active_process, *child = NULL;
    struct linux_thread *leader = NULL;
    for (unsigned i = 0; i < PROCESS_LIMIT; i++) {
        struct process *p = &processes[i];
        if (p->pid && !p->zombie && p->image == file && (p == &processes[0] || launched_pid[i] == p->pid)) {
            foreground_pid = p->pid;
            return 0;
        }
        if (!p->pid && !child) child = p;
    }
    for (unsigned i = 0; i < THREAD_LIMIT; i++) if (threads[i].state == UNUSED) { leader = &threads[i]; break; }
    if (!child || !leader || last_tid == INT32_MAX) return -11;
    child->vm = acquire_space();
    if (!child->vm) return -12;
    activate(child);
    child->image = file;
    seL4_UserContext ctx = {0};
    size_t length = 0;
    while (file->name[length]) length++;
    if (!load(&ctx, file, (const unsigned char *)file->name, length + 1, 0)) {
        release_space(child->vm);
        child->vm = NULL;
        activate(parent);
        return -8;
    }
    child->pid = last_tid + 1; child->pgid = parent->pgid; child->sid = parent->sid; child->executed = true;
    child->parent = parent->pid; child->zombie = false; child->death_signal = 0; child->vfork_tid = 0;
    memset(child->actions, 0, sizeof(child->actions));
    fd_limit = 1024; fd_hard_limit = MAX_FDS;
    for (unsigned i = 0; i < 3; i++)
        fds[i] = (struct descriptor){.file = &console, .writer = i != 0};
    launched_pid[child - processes] = child->pid;
    foreground_pid = child->pid;
    start_thread(leader, &ctx, 0);
    activate(parent);
    return 0;
}

static bool launcher_owns_exit(struct process *child) {
    unsigned index = child - processes;
    if (!child->pid || launched_pid[index] != child->pid) return false;
    if (foreground_pid == child->pid) foreground_pid = processes[0].pid;
    launched_pid[index] = 0;
    child->pid = 0;
    return true;
}

static void append_answer(char *out, size_t n, const char *s) {
    while (n > 1 && *out) { out++; n--; }
    while (n > 1 && *s) { *out++ = *s++; n--; }
    *out = 0;
}

static void command_line(struct gui_command *c) {
    const char *s = c->line;
    c->answer[0] = 0;
    c->cursor.pos = 0;
    if (same(s, "running")) {
        for (unsigned i = 0; i < PROCESS_LIMIT; i++) {
            struct process *p = &processes[i];
            if (!p->pid || p->zombie || !p->image || (i && launched_pid[i] != p->pid)) continue;
            append_answer(c->answer, sizeof(c->answer), p->image->name);
            append_answer(c->answer, sizeof(c->answer), " ");
        }
    } else if (same(s,"status")) {
        append_answer(c->answer, sizeof(c->answer),"listening=0");
    } else if (same(s,"home") || (s[0]=='o' && s[1]=='p' && s[2]=='e' && s[3]=='n' && s[4]==' ')) {
        if (active_process->pid != foreground_pid) {
            append_answer(c->answer, sizeof(c->answer),"error background");
        } else {
            const char *path = same(s,"home") ? "/apps/home" : s+5;
            bool app = path[0]=='/' && path[1]=='a' && path[2]=='p' && path[3]=='p' && path[4]=='s' && path[5]=='/';
            const struct file *f = app ? find_file(path) : NULL;
            append_answer(c->answer, sizeof(c->answer), f && !launch_app(f) ? "ok" : "error unavailable");
        }
    } else if (!same(s,"shield") && !same(s,"get time.city")) {
        append_answer(c->answer, sizeof(c->answer),"error unsupported");
    }
    append_answer(c->answer, sizeof(c->answer),"\n");
}

static long gui_io(struct descriptor *d, uintptr_t addr, size_t n, bool write) {
    struct gui_command *c = (struct gui_command *)d->cursor;
    if (write) {
        for (size_t i = 0; i < n; i++) {
            char ch;
            if (!transfer(addr+i, &ch, 1, false)) return i ? (long)i : -14;
            if (ch == '\n') {
                c->line[c->used] = 0;
                command_line(c);
                c->used = 0;
            } else if (c->used+1 < sizeof(c->line)) c->line[c->used++] = ch;
            else { c->used = 0; return -90; }
        }
        return n;
    }
    size_t amount = 0;
    while (amount < n && c->answer[c->cursor.pos + amount]) amount++;
    if (!transfer(addr, c->answer + c->cursor.pos, amount, true)) return -14;
    c->cursor.pos += amount;
    return amount;
}
