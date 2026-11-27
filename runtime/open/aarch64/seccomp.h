// Classic BPF subset used by the upstream Linux browser policies. Accepted
// filters are immutable. TSYNC verifies thread ancestry before changing it.
struct bpf_instruction { uint16_t op; uint8_t yes, no; uint32_t k; };
struct syscall_filter {
    struct syscall_filter *parent;
    unsigned len;
    struct bpf_instruction code[4096];
};
static struct syscall_filter filter_pool[64];
static bool filter_live(const struct syscall_filter *f) {
    for (unsigned i = 0; i < THREAD_LIMIT; i++)
        if (threads[i].state != UNUSED)
            for (const struct syscall_filter *p = threads[i].filters; p; p = p->parent)
                if (p == f) return true;
    return false;
}
static bool valid_filter(struct syscall_filter *f) {
    for (unsigned i = 0; i < f->len; i++) {
        struct bpf_instruction ins = f->code[i];
        switch (ins.op) {
        case 0x20: if (ins.k % 4 || ins.k > 60) return false; break; // LD W ABS
        case 0x04: case 0x54: break; // ADD/AND K
        case 0x15: case 0x35: // JEQ/JGE K
            if (i + 1 + ins.yes >= f->len || i + 1 + ins.no >= f->len) return false;
            break;
        case 0x06: break; // RET K
        default: return false;
        }
    }
    return f->code[f->len - 1].op == 0x06;
}
static long install_filter(seL4_UserContext *r) {
    if (r->x0 != 1 || (r->x1 & ~1UL)) return -22;
    if (!caller->no_new_privs) return -13;
    struct { uint16_t count, pad[3]; uintptr_t ptr; } prog;
    if (!transfer(r->x2, &prog, sizeof(prog), false)) return -14;
    if (!prog.count || prog.count > 4096) return -22;
    unsigned total = prog.count;
    for (struct syscall_filter *f = caller->filters; f; f = f->parent) total += f->len + 4;
    if (total > 32768) return -12;
    struct syscall_filter *f = NULL;
    for (unsigned i = 0; i < sizeof(filter_pool)/sizeof(*filter_pool); i++)
        if (!filter_live(&filter_pool[i])) { f = &filter_pool[i]; break; }
    if (!f) return -12;
    f->len = prog.count; f->parent = caller->filters;
    if (!transfer(prog.ptr, f->code, prog.count * sizeof(f->code[0]), false)) return -14;
    if (!valid_filter(f)) return -22;
    if (r->x1 == 1) {
        for (unsigned i = 0; i < THREAD_LIMIT; i++) {
            struct linux_thread *t = &threads[i];
            if (t->state == UNUSED || t->owner != active_process) continue;
            struct syscall_filter *ancestor = caller->filters;
            while (ancestor && ancestor != t->filters) ancestor = ancestor->parent;
            if (t->filters && !ancestor) return t->tid;
        }
        for (unsigned i = 0; i < THREAD_LIMIT; i++)
            if (threads[i].state != UNUSED && threads[i].owner == active_process) {
                threads[i].filters = f; threads[i].no_new_privs = true;
            }
    } else caller->filters = f;
    return 0;
}
static uint32_t evaluate_filter(const struct syscall_filter *f, const uint32_t data[16]) {
    uint32_t a = 0;
    for (unsigned i = 0; i < f->len; i++) {
        struct bpf_instruction ins = f->code[i];
        switch (ins.op) {
        case 0x20: a = data[ins.k / 4]; break;
        case 0x04: a += ins.k; break;
        case 0x54: a &= ins.k; break;
        case 0x15: i += a == ins.k ? ins.yes : ins.no; break;
        case 0x35: i += a >= ins.k ? ins.yes : ins.no; break;
        case 0x06: return ins.k;
        }
    }
    return 0;
}
// Linux signal frame layout from the AArch64 user ABI.
struct arm_sigframe {
    unsigned char info[128];
    uint64_t flags, link, stack[3], mask[16], aligned;
    uint64_t fault, gpr[31], sp, pc, pstate, align_reserved;
    unsigned char reserved[4096];
};
_Static_assert(offsetof(struct arm_sigframe, reserved) == 592, "Linux AArch64 signal context");
static struct arm_sigframe signal_frame;
#define GP_OFF(n) (offsetof(seL4_UserContext, x##n) / sizeof(seL4_Word))
static const unsigned char register_indices[] = {
    GP_OFF(0), GP_OFF(1), GP_OFF(2), GP_OFF(3), GP_OFF(4), GP_OFF(5), GP_OFF(6), GP_OFF(7),
    GP_OFF(8), GP_OFF(9), GP_OFF(10), GP_OFF(11), GP_OFF(12), GP_OFF(13), GP_OFF(14), GP_OFF(15),
    GP_OFF(16), GP_OFF(17), GP_OFF(18), GP_OFF(19), GP_OFF(20), GP_OFF(21), GP_OFF(22), GP_OFF(23),
    GP_OFF(24), GP_OFF(25), GP_OFF(26), GP_OFF(27), GP_OFF(28), GP_OFF(29), GP_OFF(30)
};
static void resume_context(seL4_UserContext *r) {
    caller->exec_done = true;
    check("signal suspend", seL4_TCB_Suspend(caller->tcb));
    check("signal resume", seL4_TCB_WriteRegisters(caller->tcb, 1, 0, sizeof(*r) / sizeof(long), r));
}
static void fatal_signal(unsigned sig) {
    active_process->death_signal = sig;
    exit_thread(true, 128 + sig);
}
static long signal_return(seL4_UserContext *r) {
    struct arm_sigframe *f = &signal_frame;
    if (!transfer(r->sp, f, sizeof(*f), false) || f->sp >= USER_END || f->pc >= USER_END || f->pc % 4) {
        fatal_signal(11); return 0;
    }
    caller->sigmask = f->mask[0] & ~((1ULL << 8) | (1ULL << 18));
    for (unsigned i = 0; i < 31; i++) ((seL4_Word *)r)[register_indices[i]] = f->gpr[i];
    r->sp = f->sp; r->pc = f->pc; r->spsr = (r->spsr & ~0xf0000000UL) | (f->pstate & 0xf0000000UL);
    resume_context(r);
    return 0;
}
static void syscall_trap(seL4_UserContext *r, uintptr_t next_pc, unsigned reason) {
    enum { SIGSYS = 31 };
    struct { uint64_t handler, flags, restorer, mask; } act;
    memcpy(&act, active_process->actions[SIGSYS], sizeof(act));
    if (act.handler <= 1 || (caller->sigmask & (1ULL << (SIGSYS - 1)))) { fatal_signal(SIGSYS); return; }
    read_full_context(r);
    struct arm_sigframe *f = &signal_frame;
    memset(f, 0, sizeof(*f));
    int info[] = {SIGSYS, (int)reason, 1};
    memcpy(f->info, info, sizeof(info));
    memcpy(f->info + 16, &next_pc, 8);
    uint32_t call[] = {(uint32_t)r->x8, 0xc00000b7};
    memcpy(f->info + 24, call, sizeof(call));
    memcpy(f->stack, caller->altstack, sizeof(f->stack));
    f->mask[0] = caller->sigmask;
    for (unsigned i = 0; i < 31; i++) f->gpr[i] = ((seL4_Word *)r)[register_indices[i]];
    f->gpr[0] = r->x8;
    f->pc = next_pc; f->sp = r->sp; f->pstate = r->spsr;
    uint32_t fpsimd[] = {0x46508001, 528};
    memcpy(f->reserved, fpsimd, sizeof(fpsimd));
    uint64_t top = r->sp, alt = f->stack[0], size = f->stack[2];
    bool on_alt = r->sp >= alt && r->sp - alt < size;
    if ((act.flags & 0x08000000) && !(f->stack[1] & 2) && !on_alt && size < USER_END && alt < USER_END - size)
        top = alt + size;
    uintptr_t frame = (top - sizeof(*f)) & ~15UL;
    if (top < sizeof(*f) || !transfer(frame, f, sizeof(*f), true)) { fatal_signal(11); return; }
    caller->sigmask |= act.mask;
    if (!(act.flags & 0x40000000)) caller->sigmask |= 1ULL << (SIGSYS - 1);
    if (act.flags & 0x80000000) memset(active_process->actions[SIGSYS], 0, sizeof(act));
    r->x0 = SIGSYS; r->x1 = frame; r->x2 = frame + 128; r->x3 = act.handler;
    r->pc = SIGNAL_GATE; r->sp = frame;
    resume_context(r);
}
static bool check_filter(seL4_UserContext *r, uintptr_t pc, long *result) {
    uint64_t words[] = {(0xc00000b7ULL << 32) | (uint32_t)r->x8, pc + 4, r->x0,r->x1,r->x2,r->x3,r->x4,r->x5};
    uint32_t data[16];
    memcpy(data, words, sizeof(data));
    uint32_t action = 0x7fff0000;
    for (const struct syscall_filter *f = caller->filters; f; f = f->parent) {
        uint32_t next = evaluate_filter(f, data);
        if ((int32_t)(next & 0xffff0000) < (int32_t)(action & 0xffff0000)) action = next;
    }
    switch (action & 0xffff0000) {
    case 0x7fff0000: return false;
    case 0x00050000: *result = -(long)((action & 65535) > 4095 ? 4095 : (action & 65535)); return true;
    case 0x00030000: syscall_trap(r, pc + 4, action & 65535); break;
    default: fatal_signal(31); break;
    }
    *result = 0;
    return true;
}
