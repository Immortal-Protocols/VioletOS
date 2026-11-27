// Linux threads share the app's address space, file table and signal actions.
// A badged fault endpoint identifies the calling TCB. While a thread sleeps
// or waits on a futex its MCS reply object stays with that thread.
enum { THREAD_LIMIT = 256, TIMER_IRQ = 30 };
#define TIMER_BADGE (1UL << 30)
_Static_assert(THREAD_LIMIT < TIMER_BADGE, "guest thread badges must precede the timer badge");
enum thread_state { UNUSED, RUNNING, FUTEX, SLEEPING, RETRY_IO, VFORK_WAIT, CHILD_WAIT };
struct land_domain { unsigned count; struct land_policy *layers[16]; };
struct linux_thread {
    struct land_domain domain;
    char name[16];
    seL4_CPtr tcb, saved_reply;
    int tid, wait_pid;
    uintptr_t wait_status;
    bool exec_done, no_new_privs;
    struct syscall_filter *filters;
    struct process *owner;
    enum thread_state state;
    uintptr_t clear_tid, futex;
    uint32_t bitset;
    uint64_t poll_deadline, poll_watch;
    uint64_t deadline, reply[9], sigmask;
    unsigned char altstack[24];
};
static struct linux_thread threads[THREAD_LIMIT], *caller;
static int last_tid;
static seL4_CPtr guest_caps, timer_irq;
static uint64_t timer_hz;

static uint64_t ticks_now(void) {
    uint64_t now;
    __asm__ volatile("mrs %0, cntpct_el0" : "=r"(now));
    return now;
}

static void setup_timer(void) {
    seL4_CPtr notification = alloc_object(seL4_NotificationObject, 0);
    seL4_CPtr badged = slot++;
    timer_irq = slot++;
    check("timer badge", seL4_CNode_Mint(CROOT, badged, 64, CROOT, notification, 64, seL4_AllRights, TIMER_BADGE));
    check("timer IRQ", seL4_IRQControl_Get(seL4_CapIRQControl, TIMER_IRQ, CROOT, timer_irq, 64));
    check("timer notification", seL4_IRQHandler_SetNotification(timer_irq, badged));
    check("bind timer", seL4_TCB_BindNotification(seL4_CapInitThreadTCB, notification));
    __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(timer_hz));
    __asm__ volatile("msr cntp_ctl_el0, xzr; isb" ::: "memory");
    check("ack timer", seL4_IRQHandler_Ack(timer_irq));
}

static void program_timer(void) {
    uint64_t first = UINT64_MAX;
    for (unsigned i = 0; i < THREAD_LIMIT; i++)
        if (threads[i].deadline && threads[i].deadline < first)
            first = threads[i].deadline;
    if (first == UINT64_MAX) {
        __asm__ volatile("msr cntp_ctl_el0, xzr; isb" ::: "memory");
    } else {
        __asm__ volatile("msr cntp_cval_el0, %0; msr cntp_ctl_el0, %1; isb"
                         :: "r"(first), "r"(1UL) : "memory");
    }
}

static long time_deadline(uintptr_t ptr, bool absolute, bool realtime, uint64_t *out) {
    int64_t ts[2];
    if (!transfer(ptr, ts, sizeof(ts), false)) return -14;
    if (ts[0] < 0 || ts[1] < 0 || ts[1] >= 1000000000) return -22;
    uint64_t ns = ts[0] > (INT64_MAX - ts[1]) / 1000000000 ? INT64_MAX : ts[0] * 1000000000 + ts[1];
    if (absolute && realtime)
        ns = ns > boot_entropy.epoch_ns ? ns - boot_entropy.epoch_ns : 0;
    uint64_t delta = (ns / 1000000000) * timer_hz + ((ns % 1000000000) * timer_hz + 999999999) / 1000000000;
    *out = (absolute ? start_ticks : ticks_now()) + delta;
    return 0;
}

enum { FP_COPY = 1, FP_RESET = 2 }; // target seL4 AArch64 register-transfer flags
static void start_thread(struct linux_thread *t, seL4_UserContext *ctx, seL4_CPtr fp_source) {
    if (!t->tcb) {
        t->tcb = alloc_object(seL4_TCBObject, 0);
        t->saved_reply = alloc_object(seL4_ReplyObject, 0);
        seL4_CPtr sc = alloc_object(seL4_SchedContextObject, seL4_MinSchedContextBits);
        seL4_CPtr ep = slot++;
        check("thread badge", seL4_CNode_Mint(CROOT, ep, 64, CROOT, endpoint, 64, seL4_AllRights, t - threads + 1));

        check("budget", seL4_SchedControl_ConfigureFlags(bi->schedcontrol.start, sc, 10000, 10000, 0, 0, 0));
        check("scheduling", seL4_TCB_SetSchedParams(t->tcb, seL4_CapInitThreadTCB, 0, 100, sc, ep));
    }
    check("configure", seL4_TCB_Configure(t->tcb, guest_caps,
          seL4_CNode_CapData_new(0, 60).words[0], space, 0, 0, 0));
    t->filters = caller ? caller->filters : NULL;
    t->no_new_privs = caller && caller->no_new_privs;
    t->domain = caller ? caller->domain : (struct land_domain){0};
    t->owner = active_process;
    t->exec_done = false;
    memset(t->name, 0, sizeof(t->name));
    t->tid = ++last_tid;
    t->state = RUNNING;
    t->poll_deadline = 0; t->poll_watch = 0;
    t->clear_tid = 0;
    t->deadline = 0;
    t->sigmask = caller ? caller->sigmask : 0;
    memset(t->altstack, 0, sizeof(t->altstack));
    t->altstack[8] = 2; // SS_DISABLE
    if (fp_source)
        check("inherit SIMD", seL4_TCB_CopyRegisters(t->tcb, fp_source, 0, 0, 0, 0, FP_COPY));
    check("start thread", seL4_TCB_WriteRegisters(t->tcb, 1, fp_source ? 0 : FP_RESET, sizeof(*ctx) / sizeof(long), ctx));
}

static void send_result(struct linux_thread *t, long result) {
    t->reply[0] = result;
    for (unsigned i = 0; i < 9; i++) seL4_SetMR(i, t->reply[i]);
    seL4_Send(t->saved_reply, seL4_MessageInfo_new(0, 0, 0, 9));
    t->state = RUNNING;
    t->poll_watch = 0;
    t->deadline = 0;
}

// A collision can wake an extra poller. Every wake repeats the Linux
// readiness check, so collisions never deliver a spurious poll result.
static uint64_t poll_key(const void *object) {
    return UINT64_C(1) << (((uintptr_t)object * UINT64_C(0x9e3779b97f4a7c15)) >> 58);
}
static void wake_pollers(const void *object) {
    uint64_t key = poll_key(object);
    for (unsigned i = 0; i < THREAD_LIMIT; ++i) {
        struct linux_thread *t = &threads[i];
        if (t->state == RETRY_IO && (t->poll_watch & key)) send_result(t, (long)t->reply[0]);
    }
}
// Retry a blocked Linux SVC with its original arguments after the deadline.
static long retry_io(uint64_t deadline) {
    caller->state = RETRY_IO;
    caller->deadline = deadline;
    caller->reply[8] -= 4;
    return 0;
}

static void timer_fired(void) {
    uint64_t now = ticks_now();
    for (unsigned i = 0; i < THREAD_LIMIT; i++) {
        struct linux_thread *t = &threads[i];
        if (t->deadline && t->deadline <= now)
            send_result(t, t->state == RETRY_IO ? (long)t->reply[0] : t->state == FUTEX ? -110 : 0);
    }
    program_timer();
    check("timer ack", seL4_IRQHandler_Ack(timer_irq));
}

static int wake_futex(uintptr_t addr, int count, uint32_t mask) {
    int woke = 0;
    for (unsigned i = 0; i < THREAD_LIMIT && woke < count; i++) {
        struct linux_thread *t = &threads[i];
        if (t->state == FUTEX && t->owner->vm == vm && t->futex == addr && (t->bitset & mask)) {
            send_result(t, 0);
            woke++;
        }
    }
    return woke;
}

static long clone_thread(seL4_UserContext *r, uint64_t next_pc) {
    enum { VM = 0x100, FILES = 0x400, SIGHAND = 0x800, THREAD = 0x10000,
           SETTLS = 0x80000, PARENT_SETTID = 0x100000, CHILD_CLEARTID = 0x200000, CHILD_SETTID = 0x1000000 };
    unsigned required = VM | FILES | SIGHAND | THREAD;
    unsigned allowed = required | 0x200 /* FS */ | 0x40000 /* SYSVSEM */ | 0x400000 /* DETACHED */
                     | SETTLS | PARENT_SETTID | CHILD_CLEARTID | CHILD_SETTID;
    if ((r->x0 & required) != required || (r->x0 & ~((uint64_t)allowed))) return -38;
    if (!r->x1 || r->x1 % 16) return -22;
    struct linux_thread *child = NULL;
    for (unsigned i = 0; i < THREAD_LIMIT; i++)
        if (threads[i].state == UNUSED) { child = &threads[i]; break; }
    if (!child || last_tid == INT32_MAX) return -11;
    int tid = last_tid + 1;
    if ((r->x0 & PARENT_SETTID) && !transfer(r->x2, &tid, sizeof(tid), true)) return -14;
    if ((r->x0 & CHILD_SETTID) && !transfer(r->x4, &tid, sizeof(tid), true)) return -14;
    seL4_UserContext ctx = *r;
    ctx.x0 = 0;
    ctx.sp = r->x1;
    ctx.pc = next_pc;
    if (r->x0 & SETTLS) ctx.tpidr_el0 = r->x3;
    start_thread(child, &ctx, caller->tcb);
    if (r->x0 & CHILD_CLEARTID) child->clear_tid = r->x4;
    return tid;
}

static long futex_call(seL4_UserContext *r) {
    if (r->x0 % 4) return -22;
    unsigned op = r->x1 & ~(128U | 256U);
    bool realtime = (r->x1 & 256) != 0;
    uint32_t mask = op == 9 || op == 10 ? (uint32_t)r->x5 : UINT32_MAX;
    if (!mask) return -22;
    if (op == 1 || op == 10) {
        int n = wake_futex(r->x0, (int)r->x2, mask);
        program_timer();
        return n;
    }
    if (op != 0 && op != 9) return -38;
    uint32_t word;
    if (!transfer(r->x0, &word, sizeof(word), false)) return -14;
    if (word != (uint32_t)r->x2) return -11;
    uint64_t deadline = 0;
    if (r->x3) {
        long rc = time_deadline(r->x3, op == 9, realtime, &deadline);
        if (rc) return rc;
        if (deadline <= ticks_now()) return -110;
    }
    caller->futex = r->x0;
    caller->bitset = mask;
    caller->state = FUTEX;
    caller->deadline = deadline;
    return 0;
}

static long sleep_until(uintptr_t ts, bool absolute, bool realtime) {
    uint64_t deadline;
    long rc = time_deadline(ts, absolute, realtime, &deadline);
    if (rc || deadline <= ticks_now()) return rc;
    caller->state = SLEEPING;
    caller->deadline = deadline;
    return 0;
}

static void process_exited(unsigned status);

static void notify_parent_exit(int tid);
static void exit_thread(bool group, unsigned status) {
    int zero = 0;
    if (caller->clear_tid && transfer(caller->clear_tid, &zero, sizeof(zero), true))
        wake_futex(caller->clear_tid, 1, UINT32_MAX);
    caller->state = UNUSED;
    caller->deadline = 0;
    check("thread exit", seL4_TCB_Suspend(caller->tcb));
    bool last = true;
    for (unsigned i = 0; i < THREAD_LIMIT; i++) {
        if (threads[i].state == UNUSED || threads[i].owner != active_process) continue;
        if (group) {
            check("stop thread", seL4_TCB_Suspend(threads[i].tcb));
            threads[i].state = UNUSED;
            threads[i].deadline = 0;
            notify_parent_exit(threads[i].tid);
        }
        else last = false;
    }
    notify_parent_exit(caller->tid);
    if (last) process_exited(status);
    program_timer();
}
