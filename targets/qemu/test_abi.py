"""Linux ABI completeness under the Open Runtime in QEMU (`make test-abi`; local, not in CI).

A scripted viewer taps each program in targets/qemu/abi-test-apps on Home in
turn and checks what it prints in the Open Runtime's log:
  csig      C: handlers with siginfo on an alternate stack, masks and pending
            signals, SIGSEGV (unmapped and read-only) and SIGILL caught,
            a thread interrupted while it runs in user code (VFP, and Thumb IT
            and GE state, kept), SA_RESTART against EINTR in ARM and Thumb code,
            alarm, interval and POSIX timers, sigtimedwait
  cproc     C: fork, fork+execve, posix_spawn (vfork) with a pipe, an
            environment and an O_CLOEXEC descriptor, a missing program, vfork,
            a child killed by a signal, SIGCHLD, ECHILD, the kill rule
  cfault    C: a fault with no handler still ends the program with status 139
  charden   C: the limits: fork keeps d8 and FPSCR (the fork gate), a 1 us
            timer fires at most once a millisecond, unaligned LDRD is SIGBUS
            BUS_ADRALN (and an uncaught one the wait status's signal),
            SIGKILL ends a sleeping or spinning vfork child, 64 zombies at
            most, 400 vforks (thread slots reused), a hidden "#!"
            interpreter answers ENOENT like a missing one, an image the
            loader refuses fails execve with the caller alive
  cdyn      C, dynamically linked: run by /lib/ld-musl-armhf.so.1
  rsig      Rust std: a handler, std::process::Command
  goabi     Go without GODEBUG=asyncpreemptoff=1 (it execve's itself without
            it): busy goroutines preempted, os/signal, nil dereference
            recovered, os/exec. Run ABI_GO_ROUNDS times (default 3).
The image is built with VIOLET_OPEN_LDSO=1 so it ships musl's loader. A
program whose toolchain was missing is reported as skipped.
"""

from pathlib import Path
from tempfile import TemporaryDirectory
import json
import os
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "targets/qemu"))
import home_grid
import run

SCRIPT = r'''#!/usr/bin/env python3
import json, mmap, os, re, struct, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
CHECKS = json.loads(os.environ["ABI_CHECKS"])
PRESENT = json.loads(os.environ["ABI_PRESENT"])
ROUNDS = int(os.environ.get("ABI_GO_ROUNDS", "3"))

def text():
    n = struct.unpack_from("<I", log, 4)[0]
    data = bytes(log[8:4096])
    if n <= len(data):
        return data[:n].decode(errors="replace")
    s = n % len(data)
    return (data[s:] + data[:s]).decode(errors="replace")
seen = ""
def everything():
    # The log is a 4 KiB ring; keep what scrolled past.
    global seen
    t = text()
    for k in range(min(len(seen), len(t)), 0, -1):
        if seen.endswith(t[:k]):
            seen += t[k:]
            return seen
    seen += t
    return seen
def fail(why):
    print("abi test failed:", why, "\n--- log ---\n" + everything()[-6000:], file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=10.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.02)
    fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.35)

def program(name, timeout=120):
    n = home_shown()
    start = len(everything())
    open_icon(name)
    # The program itself, not a child it forked (children keep its name).
    started = re.compile(r"started /apps/" + re.escape(name) + r" as pid (\d+)")
    until(f"{name} starts", lambda: started.search(everything()[start:]), 30)
    pid = started.search(everything()[start:]).group(1)
    done = re.compile(re.escape(name) + r" \(pid " + pid + r"\) exited with status (-?\d+)")
    until(f"{name} finishes", lambda: done.search(everything()[start:]), timeout)
    out = everything()[start:]
    for line in CHECKS[name]:
        if line.startswith("re:"):
            if not re.search(line[3:], out):
                fail(f"{name}: no line matching {line[3:]!r}")
        elif line not in out:
            fail(f"{name} did not print {line!r}")
    until(f"Home after {name}", lambda: home_shown() > n)
    print(f"{name}: passed", file=sys.stderr)

until("Open Runtime ready", lambda: "[open] ready" in everything(), 60)
until("Home", lambda: "home: ready" in everything(), 60)
time.sleep(3)
for name in ("csig", "cproc", "cfault", "charden", "cdyn", "rsig"):
    if name in PRESENT:
        program(name)
if "goabi" in PRESENT:
    for round in range(ROUNDS):
        program("goabi")
'''

CHECKS = {
    "csig": [
        "csig: usr1 2 info 1 altstack 1",
        "csig: blocked 2 pending 1 unblocked 3",
        "csig: SIGSEGV at 0x10 code 1",
        "csig: write to a constant code 2",
        "csig: SIGILL 1",
        "csig: spinner interrupted 1 vfp kept",
        "csig: thumb IT/GE under 2000 signals kept",
        "csig: SA_RESTART read 1, without -1 EINTR",
        "csig: thumb svc SA_RESTART read 1, without -1 EINTR",
        "csig: alarm 1",
        "csig: interval timer 1",
        "csig: posix timer 1",
        "csig: sigtimedwait 10 then -1 EAGAIN",
        "csig done",
        r"re:csig \(pid \d+\) exited with status 0",
    ],
    "cproc": [
        "cproc: fork 1 exit 22, parent kept 1 10",
        "abichild: exec ok, parent 1",
        "cproc: fork+exec status 7",
        "cproc: spawn 0 said 'env spawned cloexec closed' status 3",
        "cproc: missing program ENOENT",
        "cproc: vfork status 5",
        "cproc: killed child signal 15",
        "cproc: child may not signal its parent 1",
        "cproc: then ECHILD, SIGCHLD 1",
        "cproc done",
        r"re:cproc \(pid \d+\) exited with status 0",
    ],
    "cfault": [
        "cfault: about to fault",
        r"re:cfault \(pid \d+\): bad memory access \(Write\) at 0x20",
        r"re:cfault \(pid \d+\) exited with status 139",
    ],
    "charden": [
        "charden: fork kept vfp child 1 parent 1",
        "charden: 1us timer floor 1",
        "charden: SIGBUS code 1 addr 1",
        "charden: uncaught SIGBUS signal 7",
        "charden: vfork child killed asleep 9 spinning 9",
        "charden: zombies capped at 64 EAGAIN 1 reaped 64, then fork 1",
        "charden: 400 vforks 1",
        "charden: hidden interpreter ENOENT, missing ENOENT",
        "charden: refused image ENOEXEC, caller lived 1",
        "charden done",
        r"re:charden \(pid \d+\) exited with status 0",
    ],
    "cdyn": [
        "cdyn: AT_BASE set, tls 42, sqrt 1.414, malloc 1",
        "cdyn: dlopen libc ok, dlsym ok",
        "cdyn done",
        r"re:cdyn \(pid \d+\) exited with status 0",
    ],
    "rsig": [
        "rsig: handler ran 1",
        "rsig: child exit Some(4)",
        'rsig: child said "child piped"',
        "rsig: missing program refused true",
        "rsig done",
    ],
    "goabi": [
        r"re:pid \d+ execve /apps/goabi",
        "goabi: running without GODEBUG true",
        "goabi: preempted busy loops true",
        "goabi: os/signal received user defined signal 1",
        "goabi: recovered: runtime error: invalid memory address or nil pointer dereference",
        'goabi: child said "child go" err <nil>',
        "goabi done",
    ],
}


def image_files(path):
    data, at, names = path.read_bytes(), 0, set()
    while data[at:at + 6] == b"070701":
        size, namesize = int(data[at + 54:at + 62], 16), int(data[at + 94:at + 102], 16)
        names.add(data[at + 110:at + 110 + namesize - 1].decode())
        at = (((at + 110 + namesize + 3) & ~3) + size + 3) & ~3
    return names


with TemporaryDirectory() as tmp:
    viewer = Path(tmp) / "input.py"
    original = run.ROOT / "build/sel4/qemu"
    if not (original / "initfs.cpio").exists():
        raise SystemExit("Build the QEMU image first (make test-abi builds it with the test programs).")
    viewer.write_text(home_grid.viewer(SCRIPT, (original / "initfs.cpio").read_bytes()))
    viewer.chmod(0o755)
    files = image_files(original / "initfs.cpio")
    apps = [p.name for p in sorted((ROOT / "targets/qemu/abi-test-apps").iterdir()) if (p / "app.txt").exists()]
    present = [a for a in apps if f"apps/{a}" in files]
    if "apps/abichild" not in files:
        raise SystemExit("The image lacks /apps/abichild, which the process tests start (is the musl sysroot built?)")
    if "cdyn" in present and "lib/ld-musl-armhf.so.1" not in files:
        raise SystemExit("cdyn is in the image without /lib/ld-musl-armhf.so.1 (build it with VIOLET_OPEN_LDSO=1)")
    for name in sorted(set(apps) - set(present)):
        print(f"skipping {name}: its toolchain was not available when the image was built", file=sys.stderr)
    image = Path(tmp) / "image"
    image.mkdir()
    for name in ("zImage", "handoff.dtb", "entry.bin", "initfs.cpio"):
        (image / name).symlink_to(original / name)
    run.IMAGE = image
    os.environ.update(ABI_CHECKS=json.dumps(CHECKS), ABI_PRESENT=json.dumps(present))
    status = run.main(str(viewer))
    if status != 0:
        raise SystemExit(f"Linux ABI test failed (viewer status {status})")
    print("Linux ABI verified:", ", ".join(present))
