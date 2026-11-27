// Session and process group identities are inherited across fork and exec.
static struct process *process_by_pid(int pid) {
    if (!pid) return active_process;
    for (unsigned i = 0; i < PROCESS_LIMIT; i++)
        if (processes[i].pid == pid) return &processes[i];
    return NULL;
}
static long set_group(int pid, int pgid) {
    if (pgid < 0) return -22;
    struct process *p = process_by_pid(pid);
    if (!p || (p != active_process && p->parent != active_process->pid)) return -3;
    if (p != active_process && p->executed) return -13;
    if (p->sid == p->pid || p->sid != active_process->sid) return -1;
    if (!pgid) pgid = p->pid;
    if (pgid != p->pid) {
        bool exists = false;
        for (unsigned i = 0; i < PROCESS_LIMIT; i++)
            if (processes[i].pid && processes[i].pgid == pgid && processes[i].sid == p->sid) exists = true;
        if (!exists) return -1;
    }
    p->pgid = pgid;
    return 0;
}
static long new_session(void) {
    int pid = active_process->pid;
    for (unsigned i = 0; i < PROCESS_LIMIT; i++)
        if (processes[i].pid && processes[i].pgid == pid) return -1;
    active_process->pgid = active_process->sid = pid;
    return pid;
}

// Deliver uncaught termination signals immediately, even to a CPU-bound thread.
// Caught signals require a Linux signal frame and are reported as unsupported.
static long process_signal(struct process *p, struct linux_thread *target, unsigned sig) {
    if (!sig || p->zombie) return 0;
    uint64_t handler;
    memcpy(&handler, p->actions[sig], sizeof(handler));
    if (sig != 9) {
        if (handler == 1) return 0;
        if (handler || ((sig >= 19 && sig <= 22))) return -95;
        if (sig == 17 || sig == 18 || sig == 23 || sig == 28) return 0;
        if (target && (target->sigmask & (1ULL << (sig - 1)))) return -95;
    }
    struct process *original = active_process;
    struct linux_thread *sender = caller;
    struct linux_thread *victim = NULL;
    for (unsigned i = 0; i < THREAD_LIMIT; i++) {
        struct linux_thread *t = &threads[i];
        if (t->state != UNUSED && t->owner == p &&
            (sig == 9 || !(t->sigmask & (1ULL << (sig - 1))))) {
            victim = t;
            break;
        }
    }
    if (!victim) return -95;
    activate(p); caller = victim; p->death_signal = sig;
    exit_thread(true, 128 + sig);
    activate(original); caller = sender;
    return 0;
}
static long kill_process(int pid, unsigned sig) {
    if (sig > 64) return -22;
    if (pid > 0) {
        struct process *p = process_by_pid(pid);
        return p ? process_signal(p, NULL, sig) : -3;
    }
    bool matched = false, delivered = false;
    long error = -3;
    int pgid = pid < -1 ? -(int64_t)pid : active_process->pgid;
    if (pid == INT32_MIN) return -3;
    for (unsigned i = 0; i < PROCESS_LIMIT; i++) {
        struct process *p = &processes[i];
        if (!p->pid || (pid == -1 ? (p == active_process || p == &processes[0]) : p->pgid != pgid)) continue;
        matched = true; error = process_signal(p, NULL, sig); delivered |= !error;
    }
    return delivered ? 0 : matched ? error : -3;
}
static long kill_thread(int pid, int tid, unsigned sig) {
    if (pid < 0 || tid <= 0 || sig > 64) return -22;
    for (unsigned i = 0; i < THREAD_LIMIT; i++) {
        struct linux_thread *t = &threads[i];
        if (t->state != UNUSED && t->tid == tid && (!pid || t->owner->pid == pid))
            return process_signal(t->owner, t, sig);
    }
    return -3;
}
