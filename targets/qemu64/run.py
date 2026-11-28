"""Boot the qemu64 image and show its serial console.

    python3 targets/qemu64/run.py [--out DIR] [--seconds N] [--expect]

Stops when the bootstrap task prints "qemu64: done", or after N seconds.
With --expect, exits non-zero unless every line in EXPECTED was printed, in order.
"""
import argparse
import json
from pathlib import Path
import selectors
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(REPO / "targets/board"))
import board as boards

# What a correct boot prints, in this order (other lines may come between).
EXPECTED = [
    "VioletOS qemu64: seL4 aarch64 kernel up, bootstrap task running",
    "hello from a static AArch64 Linux program",
    "argc and pid: ok",
    "x7 never selects a kernel call: ok",
    "registers, FP and TPIDR_EL0 survive a call: ok",
    "brk and mmap give zeroed writable memory: ok",
    "writable and executable is refused: ok",
    "writev: two parts",
    "writev with 64-bit iovec: ok",
    "monotonic clock with 64-bit timespec: ok",
    "qemu64: unhandled Linux call 441",
    "EFAULT, EBADF and ENOSYS: ok",
    "qemu64: pid 1 exited with status 0",
    "writing to the program's code",
    "qemu64: pid 2 stopped: write fault at 0x400000,",
    "hello from musl on AArch64: hello-c, 1 argument",
    "thread-local storage: ok",
    "malloc and free: ok",
    "clock_gettime: ok",
    "no file system: fopen fails with ENOSYS: ok",
    "64-bit: sizeof(long) 8, sizeof(void *) 8, pid 3",
    "qemu64: pid 3 exited with status 0",
    "qemu64: done",
]


def missing(lines, expected=EXPECTED):
    """The first expected line not found in order, or None when all are there."""
    position = 0
    for want in expected:
        while position < len(lines) and want not in lines[position]:
            position += 1
        if position == len(lines):
            return want
        position += 1
    return None


def boot(out, seconds):
    out = Path(out)
    plan = json.loads((out / "qemu64.json").read_text())
    command = boards.emulator(boards.load(plan["board"]), [(out / plan["image"], plan["load"])], plan["entry"])
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    deadline, pending, lines = time.time() + seconds, b"", []
    try:
        while time.time() < deadline and process.poll() is None:
            if not selector.select(timeout=0.2):
                continue
            chunk = process.stdout.read1(4096)
            if not chunk:
                break
            pending += chunk
            *whole, pending = pending.split(b"\n")
            for raw in whole:
                line = raw.decode(errors="replace").rstrip("\r")
                lines.append(line)
                print(line, flush=True)
            if any(line.strip() == "qemu64: done" for line in lines):
                break
    finally:
        process.kill()
        process.wait()
    return lines


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, default=REPO / "build/sel4/qemu64")
    parser.add_argument("--seconds", type=float, default=20)
    parser.add_argument("--expect", action="store_true")
    args = parser.parse_args()
    lines = boot(args.out, args.seconds)
    if args.expect:
        want = missing(lines)
        if want:
            sys.exit(f"qemu64: the console never showed: {want}")
        print("qemu64: every expected line was printed")


if __name__ == "__main__":
    main()
