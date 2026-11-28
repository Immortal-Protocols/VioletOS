#!/usr/bin/env python3
"""Source checks for the Open Runtime's fault containment and the process
manager's frame rules (`make test-unit`; under a second, reads files only).

The behaviour is tested where it can run on the host: runtime/open/tests/
tcb_host.rs and memprot_host.rs, system/supervisor/test_open_recover.c. What
those cannot see is that the pieces stay joined across files that are built
only for the target: this pins those joins, so a later edit that undoes one
fails here instead of on a phone.
"""
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
failures = []


def check(ok, what):
    if not ok:
        failures.append(what)


def before(text, first, second):
    """Both are there, `first` first."""
    return first in text and second in text and text.index(first) < text.index(second)


def source(path):
    return (REPO / path).read_text()


def code(text):
    """Rust source without its tests, comments or string contents."""
    text = text.split("#[cfg(test)]\nmod tests")[0]
    text = re.sub(r"//[^\n]*", "", text)
    return re.sub(r'"(?:[^"\\]|\\.)*"', '""', text)


def body(text, start):
    """The text of the item that begins at `start`, to its closing brace ("" if it is gone)."""
    if start not in text:
        check(False, f"not found: {start.strip()}")
        return ""
    at = text.index(start)
    depth, i = 0, text.index("{", at)
    while True:
        depth += {"{": 1, "}": -1}.get(text[i], 0)
        i += 1
        if depth == 0:
            return text[at:i]


# 1. The code that decides what survives a fault, and the frame ledger, cannot
#    themselves panic: no unwrap, expect, assertion or indexing that can fail.
for path in ["runtime/open/contain.rs"]:
    text = re.sub(r"#\[[^\]]*\]", "", code(source(path)))  # attributes are not indexing
    for bad in [".unwrap()", ".expect(", "panic!", "assert!", "assert_eq!", "unreachable!", "todo!", "unimplemented!", "["]:
        check(bad not in text, f"{path}: `{bad}` in code that runs while recovering from a fault")
frames = code(source("system/supervisor/frames.rs"))
for bad in [".unwrap()", ".expect(", "panic!", "assert!", "assert_eq!", "unreachable!", "todo!", "unimplemented!"]:
    check(bad not in frames, f"system/supervisor/frames.rs: `{bad}`")

# 2. The runtime asks which start this is before it touches anything else,
#    and puts itself where a warm start will find it.
mod = source("runtime/open/mod.rs")
run = body(mod, "pub fn run() -> ! {")
for later in ["Log::init()", "heap::HEAP.add", "Runtime::new()"]:
    check(before(run, "contain::STATE.enter()", later), f"mod.rs run(): contain::STATE.enter() must come before {later}")
check("contain::Entry::Boot => halt()" in run, "mod.rs run(): a fault during boot must stay down, suspended")
halt = body(mod, "fn halt() -> ! {")
check("sys::tcb_suspend(sys::SLOT_AUTHORITY)" in halt, "mod.rs halt(): suspends the runtime's own thread (not a yield spin)")
check(run.count("contain::STATE.settle(") == 1 and before(run, "Box::leak", "contain::STATE.settle("),
      "mod.rs run(): the runtime is leaked onto the heap, then settled")
check(run.rstrip().endswith("rt.run()\n}"), "mod.rs run(): ends in the event loop")

# 3. Work for an app is marked, and the mark is cleared after every fault handled.
handler = body(mod, "    fn handle_fault(&mut self")
check(before(handler, "contain::STATE.serve(index)", "match r.label"),
      "mod.rs handle_fault: serve(index) before the fault is handled")
calls = [m.end() for m in re.finditer(r"self\.handle_fault\(r\);", mod)]
check(len(calls) >= 2, "mod.rs: handle_fault call sites not found")
for at in calls:
    check(mod[at:at + 80].lstrip().startswith("contain::STATE.done();"), "mod.rs: contain::STATE.done() must follow every handle_fault(r)")
recover = body(mod, "    fn recover(&mut self")
check("contain::recover(self, served)" in recover, "mod.rs recover(): runs the shipped contain::recover (what tcb_host.rs tests)")
nothing = recover[recover.index("contain::Verdict::Nothing"):] if "contain::Verdict::Nothing" in recover else ""
check("halt()" in nothing, "mod.rs recover(): a fault outside any app's call stops the runtime")
check("self.k.replies" in recover, "mod.rs recover(): an empty MCS Reply slot is refilled from the reserved Reply untyped")
host = body(mod, "impl contain::Host for Runtime {")
check("self.reap(i)" in host and "sys::revoke(" in host and "Runtime::start_home(self, false)" in host,
      "mod.rs contain::Host: close reaps, revoke revokes, and only Home is restarted (not the agent)")

# 4. The supervisor restarts the runtime only on a real fault message, at the
#    package entry on the runtime's own stack, and within the budget.
sup = source("system/supervisor/supervisor.c")
restart = body(sup, "static bool open_warm_restart(void) {")
check("!open_faulted" in restart and "open_warm_allowed(" in restart, "supervisor.c: warm restart needs a fault message and the budget")
check(".pc = BASE" in restart and ".sp = OPEN_STACK_TOP" in restart, "supervisor.c: warm restart at the entry, on the runtime's stack")
check(restart.count("seL4_TCB_WriteRegisters(procs[OPEN_RT].task.thread") == 1, "supervisor.c: only the Open Runtime's thread is restarted")
check(sup.count("open_warm_restart()") == 1, "supervisor.c: one place decides to warm-restart")
block = body(sup, "if (badge == 16 + OPEN_RT && source != 1) {")
check(before(block, "open_warm_restart()", "seL4_TCB_Suspend(procs[OPEN_RT].task.thread)"),
      "supervisor.c: the Open Runtime fault block tries a warm restart, then suspends")
lines = sup.splitlines()
assigned = [n for n, line in enumerate(lines) if re.search(r"\bopen_faulted = ", line)]
check(len(assigned) == 1 and "open_faulted = badge == 16 + OPEN_RT;" in lines[assigned[0]]
      and re.search(r"seL4_MessageInfo_t msg = \w+\(life, &badge", lines[assigned[0] - 1]),
      "supervisor.c: open_faulted is set from the received badge, right after the receive and before the stall check can set one")
stall = next((n for n, line in enumerate(lines) if "stalled(limit)" in line), 0)
check(assigned and assigned[0] < stall, "supervisor.c: open_faulted is decided before the stall check")
header = source("system/supervisor/open_recover.h")
limit = int(re.search(r"OPEN_WARM_LIMIT = (\d+)", header)[1])
check(1 <= limit <= 5, "open_recover.h: a fault loop must end after a few restarts")
total = int(re.search(r"OPEN_WARM_TOTAL = (\d+)", header)[1]) if "OPEN_WARM_TOTAL = " in header else 0
check(limit <= total <= 16 and "w->total >= OPEN_WARM_TOTAL" in header, "open_recover.h: a cap on restarts per boot, however the faults are paced")

# 5. Frames are named in one place. Only the process manager turns a pool page
#    into an app mapping, only after the ledger agreed, and only the runtime's
#    boot says which pages are the apps'.
open_dir = REPO / "runtime/open"
procmgr = source("system/supervisor/procmgr.rs")
for f in sorted(open_dir.glob("*.rs")):
    text = code(f.read_text())
    if f.name not in ("pool.rs", "sys.rs"):
        check(not re.search(r"\bpage_map\(", text), f"runtime/open/{f.name}: maps a page itself (only procmgr.rs and the pool may)")
    if f.name != "pool.rs":
        check(not re.search(r"\bpool\.cap\(", text), f"runtime/open/{f.name}: names a pool page capability (only procmgr.rs may)")
pm = code(procmgr)
check(len(re.findall(r"\bpool\.cap\(", pm)) == 1 and "pool.cap(" in body(procmgr, "    fn map_claimed("),
      "procmgr.rs: pool page capabilities are copied in map_claimed only")
check(len(re.findall(r"\bmap_claimed\(", pm)) == 2, "procmgr.rs: map_claimed is called from map only")
mapper = body(procmgr, "    pub fn map(&mut self")
check(before(mapper, "ledger().claim(self.pd, index)", "self.map_claimed("), "procmgr.rs map: the ledger is asked first")
check("va >= self.layout.user_top" in mapper and "ledger().release(self.pd, index)" in mapper, "procmgr.rs map: app addresses only; a failed mapping lets the page go")
check("ledger().release(self.pd, page.pool)" in body(procmgr, "    fn unmap_page("), "procmgr.rs unmap_page: releases the page in the ledger")
check(len(re.findall(r"\bpool\.give_back\(", pm)) == 1 and "pool.give_back(" in body(procmgr, "fn give(pool"),
      "procmgr.rs: pages go back to the pool only through give(), which keeps a held page out")
check(len(re.findall(r"\bpool\.take\(", pm)) == 1 and "pool.take(" in body(procmgr, "fn fresh(pool"),
      "procmgr.rs: pages come from the pool only through fresh(), which passes over a held page before any write")
fork = body(procmgr, "    pub fn fork_into(")
check(before(fork, "fresh(pool)", "copy_nonoverlapping"), "procmgr.rs fork_into: the ledger is asked before the parent's page is copied")
check("frames::Frames::new(0, 0)" in body(procmgr, "#[cfg(not(test))]\npub fn ledger()"), "procmgr.rs: the runtime's ledger starts closed")
check(len(re.findall(r"ledger\(\)\.limit\(", code(mod))) == 1 and "ledger().limit(" in body(mod, "    fn new() -> Result<Runtime, i32> {"),
      "mod.rs: the apps' page range is set once, at boot")
for f in sorted(open_dir.glob("*.rs")):
    if f.name != "mod.rs":
        check("ledger().limit(" not in code(f.read_text()), f"runtime/open/{f.name}: changes which pages apps may map")

if failures:
    print("\n".join("FAIL " + f for f in failures))
    sys.exit(1)
print("open containment and frame rules: source checks ok")
