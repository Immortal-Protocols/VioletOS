// A process owns its descriptor table, signals and seL4 address space.
// vfork holds the calling parent's reply until the child execs or exits.
// exec loads a new address space first; a failed load keeps the old image.
static struct vm_space *acquire_space(void) {
    for (unsigned i = 0; i < PROCESS_LIMIT + 1; i++) {
        struct vm_space *v = &spaces[i];
        if (v->users) continue;
        if (!v->cap) {
            v->cap = alloc_object(seL4_ARM_VSpaceObject, 0);
            check("process ASID", seL4_ARM_ASIDPool_Assign(seL4_CapInitThreadASIDPool, v->cap));
        }
        v->users = 1;
        v->count = 0;
        v->next = 0x200000000ULL;
        v->brk = 0x80000000UL;
        return v;
    }
    return NULL;
}
static void release_space(struct vm_space *v) {
    if (--v->users) return;
    for (unsigned i = 0; i < npages; i++) {
        struct page *p = &pages[i];
        if (p->owner != v) continue;
        forget_page(p);
    }
    while (v->table_count) {
        seL4_CPtr table = v->tables[--v->table_count];
        check("process page table unmap", seL4_ARM_PageTable_Unmap(table));
        if (free_table_count < sizeof(free_tables) / sizeof(*free_tables)) free_tables[free_table_count++] = table;
        else check("release table capability", seL4_CNode_Delete(CROOT, table, 64));
    }
}
static void drop_descriptor(struct descriptor *d) {
    tty_ref(d->file, -1);
    struct local_socket *local = local_of(d->file);
    if (local) local_release(local);
    if (event_of(d->file)) --event_of(d->file)->refs;
    struct land_policy *policy = land_fd(d->file);
    if (policy && !--policy->refs) land_collect(policy);
    if (d->pipe) {
        if (d->writer) d->pipe->writers--; else d->pipe->readers--;
        if (!d->pipe->writers || !d->pipe->readers) wake_pollers(d->pipe);
    }
#ifdef VIOLET_NETWORK
    if (d->inet) inet_release(d->inet);
#endif
    struct tmp_node *tmp = tmp_node_of(d->file);
    if (d->cursor && !--d->cursor->refs && tmp) { --tmp->opens; tmp_collect(tmp); }
    *d = (struct descriptor){0};
}
static long close_fd(uint64_t fd) {
    if (fd >= MAX_FDS || (!fds[fd].pipe && !fds[fd].file)) return -9;
    if (fds[fd].file) wake_pollers(fds[fd].file);
    if (fds[fd].pipe) wake_pollers(fds[fd].pipe);
    if (fds[fd].cursor) release_file_locks(fds[fd].file, active_process->pid);
    drop_descriptor(&fds[fd]);
    return 0;
}
static void reference_fd(struct descriptor *d) {
    tty_ref(d->file, 1);
    struct local_socket *local = local_of(d->file);
    if (local) ++local->refs;
    if (event_of(d->file)) ++event_of(d->file)->refs;
    if (land_fd(d->file)) ++land_fd(d->file)->refs;
#ifdef VIOLET_NETWORK
    if (d->inet) d->inet->refs++;
#endif
    if (d->cursor) d->cursor->refs++;
    if (d->pipe) {
        if (d->writer) d->pipe->writers++; else d->pipe->readers++;
    }
}
static long duplicate_fd(uint64_t old, uint64_t index, bool fixed, bool cloexec) {
    if (old >= MAX_FDS || (!fds[old].file && !fds[old].pipe)) return -9;
    if (index >= fd_limit) return fixed ? -9 : -22;
    if (fixed) {
        if (old == index) return -22;
        if (fds[index].file || fds[index].pipe) close_fd(index);
    } else {
        while (index < fd_limit && (fds[index].file || fds[index].pipe)) index++;
        if (index == fd_limit) return -24;
    }
    fds[index] = fds[old];
    fds[index].cloexec = cloexec;
    reference_fd(&fds[index]);
    return index;
}

static long clone_process(seL4_UserContext *r, uint64_t pc) {
    const unsigned vm_flag = 0x100, vfork = 0x4000, set_tid = 0x100000;
    uint64_t flags = r->x0;
    bool shares_vm = flags & vm_flag;
    if ((flags & 255) != 17 || flags & ~(255ULL | vm_flag | vfork | set_tid)) return -38;
    if (shares_vm != !!(flags & vfork)) return -38;
    struct linux_thread *leader = NULL;
    struct process *child = NULL, *parent = active_process;
    for (unsigned i = 0; i < THREAD_LIMIT; i++) if (threads[i].state == UNUSED) { leader = &threads[i]; break; }
    for (unsigned i = 0; i < PROCESS_LIMIT; i++) if (!processes[i].pid) { child = &processes[i]; break; }
    if (!child || !leader || last_tid == INT32_MAX) {
        say(!child ? "process table full in " : "thread or ID limit in ");
        say(parent->image->name); say("\n");
        return -11;
    }

    if (shares_vm) {
        child->vm = parent->vm;
        ++child->vm->users;
    } else {
        child->vm = acquire_space();
        if (!child->vm) return -12;
        struct vm_space *src = parent->vm, *dst = child->vm;
        memcpy(dst->areas, src->areas, src->count * sizeof(*src->areas));
        dst->count = src->count; dst->next = src->next; dst->brk = src->brk;
        activate(child);
        unsigned count = npages;
        for (unsigned i = 0; i < count; i++) {
            if (pages[i].owner != src || !pages[i].va) continue;
            struct page *p = page_at(pages[i].va, region_at(pages[i].va));
            if (!p) { release_space(dst); activate(parent); return -12; }
            if (p->backing || pages[i].packed) continue;
            if (p->packed) {
                privatize_packed(p);
                change_page(p, region_at(p->va)->prot);
            }
            memcpy((void *)(ALIAS + (p - pages) * PAGE), (void *)(ALIAS + i * PAGE), PAGE);
            if (region_at(p->va)->prot & 4)
                check("fork instruction cache", seL4_ARM_Page_Unify_Instruction(p->cap, 0, PAGE));
        }
        activate(parent);
    }
    int pid = last_tid + 1;
    if ((flags & set_tid) && !transfer(r->x2, &pid, sizeof(pid), true)) {
        release_space(child->vm);
        return -14;
    }
    child->pid = pid; child->pgid = parent->pgid; child->sid = parent->sid; child->executed = false;
    child->creator_tid = caller->tid; child->parent_signal = 0;
    child->parent = parent->pid; child->zombie = false; child->death_signal = 0;
    child->vfork_tid = shares_vm ? caller->tid : 0;
    child->image = parent->image;
    memcpy(child->actions, parent->actions, sizeof(child->actions));
    memcpy(&child->files, &parent->files, sizeof(child->files));
    for (unsigned i = 0; i < MAX_FDS; i++) reference_fd(&child->files.entries[i]);
    seL4_UserContext ctx = *r;
    ctx.pc = pc; ctx.x0 = 0;
    if (r->x1) ctx.sp = r->x1;
    activate(child);
    start_thread(leader, &ctx, caller->tcb);
    activate(parent);
    if (shares_vm) caller->state = VFORK_WAIT;
    return pid;
}

static void release_vfork(struct process *child) {
    if (!child->vfork_tid) return;
    for (unsigned i = 0; i < THREAD_LIMIT; i++) {
        struct linux_thread *t = &threads[i];
        if (t->tid == child->vfork_tid && t->state == VFORK_WAIT) send_result(t, child->pid);
    }
    child->vfork_tid = 0;
}
static unsigned char exec_text[128 * 1024];
static long get_strings(uintptr_t vec, size_t *length) {
    if (!vec) return 0;
    for (unsigned i = 0; i <= 128; i++) {
        uintptr_t ptr;
        if (!transfer(vec + i * 8, &ptr, 8, false)) return -14;
        if (!ptr) return 0;
        if (i == 128) return -7;
        do {
            if (*length == sizeof(exec_text)) return -7;
            if (!transfer(ptr++, exec_text + *length, 1, false)) return -14;
        } while (exec_text[(*length)++]);
    }
    return -7;
}
static long exec_guest(seL4_UserContext *r) {
    char path[512];
    long error = get_path(r->x0, path, sizeof(path));
    if (error) return error;
    const struct file *f = find_file(path);
    if (!f) return -2;
    if (!land_exec(f)) return -13;
    size_t total = 0;
    if ((error = get_strings(r->x1, &total))) return error;
    size_t args = total;
    if ((error = get_strings(r->x2, &total))) return error;
    struct vm_space *original = vm, *replacement = acquire_space();
    if (!replacement) return -12;
    active_process->vm = vm = replacement;
    seL4_UserContext ctx = {0};
    if (!load(&ctx, f, exec_text, args, total - args)) {
        active_process->vm = vm = original;
        release_space(replacement);
        return -8;
    }
    for (unsigned i = 0; i < THREAD_LIMIT; i++) {
        struct linux_thread *t = &threads[i];
        if (t != caller && t->owner == active_process && t->state != UNUSED) {
            check("exec stop sibling", seL4_TCB_Suspend(t->tcb));
            t->state = UNUSED; t->deadline = 0;
        }
    }
    check("exec suspend", seL4_TCB_Suspend(caller->tcb));
    check("exec configure", seL4_TCB_Configure(caller->tcb, guest_caps,
          seL4_CNode_CapData_new(0, 60).words[0], replacement->cap, 0, 0, 0));
    release_space(original);
    active_process->image = f;
#ifdef VIOLET_TRACE
    say("exec "); hex(active_process->pid); say(" "); say(f->name); say("\n");
#endif
    active_process->executed = true;
    for (unsigned i = 0; i < MAX_FDS; i++) if (fds[i].cloexec) close_fd(i);
    for (unsigned sig = 1; sig <= 64; sig++) {
        uint64_t handler; memcpy(&handler, active_process->actions[sig], sizeof(handler));
        if (handler != 1) memset(active_process->actions[sig], 0, 32);
    }
    caller->clear_tid = 0;
    caller->tid = active_process->pid;
    memset(caller->altstack, 0, sizeof(caller->altstack)); caller->altstack[8] = 2;
    caller->exec_done = true;
    check("exec", seL4_TCB_WriteRegisters(caller->tcb, 1, FP_RESET, sizeof(ctx) / sizeof(long), &ctx));
    release_vfork(active_process);
    return 0;
}
static long reap_status(struct process *child, uintptr_t dest) {
    int status = child->status;
    if (dest && !transfer(dest, &status, sizeof(status), true)) return -14;
    long pid = child->pid;
    child->pid = 0;
    return pid;
}
static bool wait_matches(const struct process *child, int which) {
    return which == -1 || (which > 0 ? child->pid == which : child->pgid == -(int64_t)which);
}
static long wait_child(seL4_UserContext *r) {
    int pid = (int)r->x0;
    if ((r->x2 & ~1UL) || r->x3) return -22;
    if (!pid) pid = -active_process->pgid;
    bool child_exists = false;
    for (unsigned i = 0; i < PROCESS_LIMIT; i++) {
        struct process *child = &processes[i];
        if (!child->pid || child->parent != active_process->pid || !wait_matches(child, pid)) continue;
        child_exists = true;
        if (child->zombie) return reap_status(child, r->x1);
    }
    if (!child_exists) return -10;
    if (r->x2 & 1) return 0;
    caller->wait_pid = pid; caller->wait_status = r->x1;
    caller->state = CHILD_WAIT;
    return 0;
}
static void process_exited(unsigned status) {
    struct process *child = active_process;
    if (child == &processes[0]) {
        say("VioletOS AArch64: exit "); hex(status); say("\n"); halt();
    }
    child->status = child->death_signal ? child->death_signal : (status & 255) << 8;
    child->zombie = true;
    release_vfork(child);
    for (unsigned i = 0; i < MAX_FDS; i++) if (fds[i].pipe || fds[i].file) close_fd(i);
    release_space(child->vm);
    for (unsigned i = 0; i < PROCESS_LIMIT; i++)
        if (processes[i].pid && processes[i].parent == child->pid) processes[i].parent = 1;
    if (launcher_owns_exit(child)) return;
    for (unsigned i = 0; i < THREAD_LIMIT; i++) {
        struct linux_thread *t = &threads[i];
        if (t->state != CHILD_WAIT || t->owner->pid != child->parent) continue;
        if (!wait_matches(child, t->wait_pid)) continue;
        activate(t->owner);
        send_result(t, reap_status(child, t->wait_status));
        activate(child);
        break;
    }
}

static void notify_parent_exit(int tid) {
    struct process *original = active_process;
    struct linux_thread *original_caller = caller;
    for (unsigned i = 0; i < PROCESS_LIMIT; i++) {
        struct process *p = &processes[i];
        if (!p->pid || p->zombie || p->creator_tid != tid) continue;
        p->creator_tid = 0;
        if (p->parent_signal != 9) continue;
        for (unsigned j = 0; j < THREAD_LIMIT; j++) {
            if (threads[j].state == UNUSED || threads[j].owner != p) continue;
            activate(p); caller = &threads[j];
            p->death_signal = 9; exit_thread(true, 137);
            activate(original); caller = original_caller;
            break;
        }
    }
}
