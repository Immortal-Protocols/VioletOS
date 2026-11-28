"""The one QEMU end-to-end test of the local loop (`make test-system`; not in CI).

Everything else that waves 1 and 2 checked in QEMU is a host integration test
now (tests/integration, seconds; docs/test-map.md lists where each assertion
went). What is here needs the real kernel, the real MMU or the boot path:

Boot 1, from slot A of a model A/B bootloader (tools/ota_boot.py):
  - Linux ABI on the real kernel (targets/qemu/abi-test-apps): signal frames
    and register state on delivery (VFP, Thumb IT and GE state), SA_RESTART,
    timers, fork through the fork gate, execve, posix_spawn, wait statuses, a
    dynamically linked program run by musl's loader, Rust std and Go;
  - W^X on real page tables (targets/qemu/wx-test-apps/wxprobe, seven
    launches): a write to code and to a constant, running the stack and the
    heap, reading PROT_NONE memory and writing memory made read-only after
    it was mapped each end the app with status 139 and the kind of access
    in the log; the stack, heap, mmap area and AT_RANDOM move between launches;
  - the capability policy through the real system call dispatch
    (targets/qemu/policy-test-apps): the probe's own run of every check;
  - an older signed release and a signed manifest sent with another image
    refused by the supervisor's own check (verify.rs ota_verify);
  - a signed full-system update fetched over HTTPS by Settings and staged in
    slot B;
  - a passcode set in Settings on the runtime's keypad.
Boot 2, from slot B as a trial: the phone is locked from the start (the
  passcode record survived the restart), a tap where Settings is opens
  nothing, a wrong passcode is counted and the right one unlocks; the system
  comes up healthy and the supervisor has Storage mark the slot successful,
  read here from the disk.

`--arch aarch64` checks the AArch64 build of the same system instead
(targets/qemu64/stack.py; `make qemu64 QEMU64_STACK=1`, not in the loop):
one boot, the layers it reaches in order (AARCH64_LAYERS below), then a
64-bit program's signals and posix_spawn (targets/qemu64/test-apps/abi64).

The image must be the one `make test-system` builds: VIOLET_SYSTEM_VERSION=2,
VIOLET_UPDATE_URL=https://updates.violet.test/latest.vota, the test CA of
`--prepare` as VIOLET_UPDATE_TEST_ROOT, VIOLET_OPEN_LDSO=1 and the three test
app folders in VIOLET_OPEN_EXTRA_APPS. A program whose toolchain was missing
when the image was built is reported as skipped.
"""
from functools import partial
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from tempfile import TemporaryDirectory
import json
import os
import ssl
import subprocess
import sys
import threading

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "image/packages"))
sys.path.insert(0, str(ROOT / "targets/qemu"))
import home_grid  # noqa: E402
import ota_boot  # noqa: E402
import ota_bundle  # noqa: E402
import run  # noqa: E402
import sign  # noqa: E402

STATE = ROOT / "build/tests/ota/qemu-tls"
HOST = "updates.violet.test"
SYSTEM_VERSION = 2

SCRIPT = r"""#!/usr/bin/env python3
import json, mmap, os, re, struct, sys, time
sys.path.insert(0, os.environ["OTA_TOOLS"])
import ota_boot
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
CHECKS = json.loads(os.environ["ABI_CHECKS"])
PRESENT = json.loads(os.environ["ABI_PRESENT"])
STAGE = os.environ["SYSTEM_STAGE"]
DISK = os.environ["OTA_DISK"]
SERVE = os.environ["OTA_SERVE"]

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
    print("system test failed:", why, "\n--- log ---\n" + everything()[-6000:], file=sys.stderr)
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
def control():
    return ota_boot.read_control(DISK)
# The runtime's keypad (runtime/open/sysui.rs pad_rect): 1-9, DEL 0 OK, CANCEL.
def key(k):
    n = {"DEL": 9, "0": 10, "OK": 11}[k] if k in ("DEL", "0", "OK") else int(k) - 1
    tap(40 + n % 3 * 220 + 100, 800 + n // 3 * 160 + 70)
    time.sleep(0.35)  # past the pad's anti-tapjacking quiet time
def enter(pin):
    for d in pin:
        key(d)
    key("OK")

until("Open Runtime ready", lambda: "[open] ready" in everything(), 90)
until("Home", lambda: "home: ready" in everything(), 120)

if STAGE == "trial":
    # Boot 2: the slot the update went to, running as a trial.
    slot = control().suffix()
    if slot != 1:
        fail(f"the trial is running from slot {slot}, not B")
    # The passcode set in boot 1 was kept on /data (writeback.rs, data.rs
    # fsync): the phone is locked from the start and a tap reaches no app.
    until("locked at boot", lambda: "trusted ui: locked" in everything(), 60)
    time.sleep(1)
    tap(*HOME_ICONS["settings"])
    time.sleep(2)
    if "settings: ready" in everything():
        fail("a tap on the lock screen opened Settings")
    enter("0000")
    until("a wrong passcode is counted", lambda: "trusted ui: wrong passcode" in everything(), 60)
    if "trusted ui: unlocked" in everything():
        fail("a wrong passcode unlocked the phone")
    time.sleep(1)
    enter("1357")
    until("the passcode unlocks", lambda: "trusted ui: unlocked" in everything(), 60)
    print("lock: the passcode survived the restart; locked at boot, a tap opened nothing, the right passcode unlocked", file=sys.stderr)
    until("the supervisor marks the slot successful", lambda: control().slot(slot)["successful"], 90)
    print("update: slot B marked successful after a healthy boot", file=sys.stderr)
    # The power key (runtime/open/power.rs; was targets/qemu/test_lock.py): a
    # short press turns the screen off and, with a passcode set, locks; the
    # next turns it on to the lock screen.
    low = mmap.mmap(ram.fileno(), 0x4000, offset=0x49000000 - 0x40000000)
    def power():
        # targets/qemu/power.rs words 17 (held) and 18 (presses); 13 bit 0 the wake line.
        struct.pack_into("<I", low, 0x2000 + 17 * 4, 1)
        struct.pack_into("<I", low, 0x2000 + 18 * 4, struct.unpack_from("<I", low, 0x2000 + 18 * 4)[0] + 1)
        struct.pack_into("<I", low, 0x2000 + 13 * 4, struct.unpack_from("<I", low, 0x2000 + 13 * 4)[0] | 1)
        time.sleep(0.15)
        struct.pack_into("<I", low, 0x2000 + 17 * 4, 0)
    before = everything().count("screen off, locked")
    power()
    until("the power key turns the screen off and locks", lambda: everything().count("screen off, locked") > before, 15)
    before = everything().count("screen on")
    time.sleep(0.5)
    power()
    until("the power key turns the screen on", lambda: everything().count("screen on") > before, 15)
    print("power key: screen off locks, the next press turns it on", file=sys.stderr)
    sys.exit(0)

if "entropy: seeded" not in everything() and "too little timer jitter" in everything():
    fail("The random generator was not seeded")
time.sleep(3)

# ---- 1. Linux ABI on the real kernel (was targets/qemu/test_abi.py) ----
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
    print(f"abi: {name} passed", file=sys.stderr)

for name in ("csig", "cproc", "cfault", "charden", "cdyn", "rsig", "goabi"):
    if name in PRESENT:
        program(name)

# ---- 2. W^X on real page tables (was targets/qemu/test_wx.py) ----
PROBES = [("write-code", "Write"), ("write-constant", "Write"), ("run-stack", "Exec"),
          ("run-heap", "Exec"), ("read-none", "Read"), ("write-readonly", "Write")]
layouts = []
ended = re.compile(r"wxprobe \(pid (\d+)\) exited with status (-?\d+)")
for n in range(len(PROBES) + 1):
    shown = home_shown()
    start = len(everything())
    open_icon("wxprobe")
    until(f"launch {n} of the probe did not end", lambda: ended.search(everything()[start:]) and home_shown() > shown, 45)
    out = everything()[start:]
    if "wx: FAIL" in out:
        fail("A refusal the policy promises did not happen")
    if "SURVIVED" in out:
        fail("The hardware let a forbidden access through")
    layout = re.search(rf"wx {n}: layout main=(\S+) stack=(\S+) heap=(\S+) mmap=(\S+) random=([0-9a-f]{{32}})", out)
    if f"wx {n}: checks ok" not in out or not layout:
        fail(f"launch {n}: the checks did not all pass")
    layouts.append(layout.groups())
    status = ended.search(out).group(2)
    if n < len(PROBES):
        name, access = PROBES[n]
        probe = re.search(rf"wx {n}: probe ([a-z-]+)", out)
        fault = re.search(r"wxprobe \(pid \d+\): bad memory access \((\w+)\) at", out)
        if not probe or probe.group(1) != name or not fault or fault.group(1) != access or status != "139":
            fail(f"launch {n}: expected {name} to end the app on a {access} fault with status 139")
    elif f"wx {n}: all probes done" not in out or status != "0":
        fail(f"launch {n}: the harmless run did not exit cleanly (status {status})")
    time.sleep(.3)
mains, stacks, heaps, mmaps, randoms = (set(column) for column in zip(*layouts))
# 19 bits for the stack and 128 for AT_RANDOM never repeat in seven launches;
# mmap has 12 bits and the heap 8, so allow a rare repeat there.
if len(stacks) != len(layouts) or len(randoms) != len(layouts) or len(mmaps) < len(layouts) - 1 or len(heaps) < 3:
    fail(f"The address-space layout did not vary: {layouts}")
if any(r == "00" * 16 for r in randoms):
    fail("AT_RANDOM is zero")
print(f"wx: six forbidden accesses stopped by the hardware; {len(stacks)} stacks, {len(heaps)} heaps, "
      f"{len(mmaps)} mmap areas in {len(layouts)} launches", file=sys.stderr)

# ---- 3. The capability policy through the real dispatch (was targets/qemu/test_policy.py) ----
shown = home_shown()
start = len(everything())
open_icon("victim")
until("the victim did not write its files", lambda: "victim: wrote" in everything()[start:], 60)
until("the victim did not go Home", lambda: home_shown() > shown, 30)
start = len(everything())
open_icon("probe")
until("the probe did not finish", lambda: re.search(r"policy: (ok|FAILED) \d+ checks", everything()[start:]), 90)
out = everything()[start:]
failed = re.findall(r"policy: FAIL [^\n]*", out)
if failed or "policy: FAILED" in out:
    fail("\n".join(failed) or "the probe failed")
refused = set(re.findall(r"policy: probe \(pid \d+\) may not use (\S+)", out))
for cap in ("net", "mic", "system", "wifi-admin"):
    if cap not in refused:
        fail(f"the runtime did not log the refused {cap} (saw {sorted(refused)})")
until("Home after the probe", lambda: home_shown() > shown + 1, 30)
print("policy:", re.findall(r"policy: ok \d+ checks", out)[-1], file=sys.stderr)

# ---- 4. A signed update staged in the other slot (was targets/qemu/test_ota.py, boot 1) ----
time.sleep(2)
open_icon("settings")
until("Settings starts", lambda: "settings: ready" in everything(), 60)
time.sleep(1)
tap(360, 250 + 7 * 124 + 54)  # Updates, the menu's eighth row
until("Updates opens", lambda: "settings: updates, system=Some(%s)" % os.environ["OTA_SYSTEM"] in everything(), 30)
def install(name, expect, timeout):
    before = everything().count("settings: update ")
    open(SERVE, "w").write(name)
    tap(360, 955)  # Download and install
    until(f"{name}: {expect}", lambda: everything().count("settings: update ") > before and expect in everything()[-4000:], timeout)
    print(f"update: {name}: {expect}", file=sys.stderr)
# The supervisor's own check (verify.rs ota_verify, called by supervisor.c
# with this build's floor, keys and target, and on apply with the digest
# Storage read back) refuses an older release and a swapped image.
install("old.vota", "older than this system", 120)
install("forged.vota", "does not match its signature", 300)
install("good.vota", "update staged version=2", 900)
time.sleep(2)

# ---- 5. A passcode, for the next boot (was targets/qemu/test_trusted_ui.py) ----
n = everything().count("settings: Menu")
tap(108, 63)  # Back (apps/lib/violet HOME_BOX) to the Settings menu
until("the Settings menu", lambda: everything().count("settings: Menu") > n, 10)
time.sleep(0.5)
tap(360, 250 + 5 * 124 + 54)  # Passcode, the menu's sixth row
until("the Passcode page", lambda: "settings: Passcode" in everything(), 10)
time.sleep(0.5)
tap(360, 1060)  # Set passcode (settings pages.rs PASS_SET)
until("the runtime's passcode pad", lambda: "trusted ui: passcode pad" in everything(), 10)
time.sleep(0.6)
enter("1357")
enter("1357")
until("the passcode is saved", lambda: "trusted ui: passcode set" in everything(), 60)
print("passcode: set on the system keypad", file=sys.stderr)
time.sleep(2)
"""

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


def openssl(*args):
    subprocess.run(["openssl", *args], check=True, capture_output=True)


def prepare():
    """The test CA and server certificate; prints the CA (DER, hex) for the image build."""
    STATE.mkdir(parents=True, exist_ok=True)
    ca, key = STATE / "ca.pem", STATE / "ca-key.pem"
    if not ca.exists():
        openssl("ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", str(key))
        openssl("req", "-x509", "-new", "-sha256", "-key", str(key), "-days", "3650", "-subj", "/CN=VioletOS OTA test CA",
                "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign",
                "-out", str(ca))
        server_key, csr, ext = STATE / "server-key.pem", STATE / "server.csr", STATE / "server.ext"
        openssl("ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", str(server_key))
        openssl("req", "-new", "-key", str(server_key), "-subj", f"/CN={HOST}", "-out", str(csr))
        ext.write_text(f"subjectAltName=DNS:{HOST}\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n")
        # SHA-256 explicitly: LibreSSL signs with ECDSA-SHA1 otherwise, which rustls refuses.
        openssl("x509", "-req", "-sha256", "-in", str(csr), "-CA", str(ca), "-CAkey", str(key), "-CAcreateserial",
                "-days", "3650", "-extfile", str(ext), "-out", str(STATE / "server.pem"))
    der = subprocess.run(["openssl", "x509", "-in", str(ca), "-outform", "DER"], check=True, capture_output=True).stdout
    print(der.hex())


class Server(ThreadingHTTPServer):
    daemon_threads = True


def https_server(directory, serve_file):
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            name = serve_file.read_text().strip() if serve_file.exists() else ""
            path = directory / name
            if self.path != "/latest.vota" or not name or not path.is_file():
                self.send_error(404)
                return
            data = path.read_bytes()
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Content-Type", "application/octet-stream")
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *a):
            pass

    server = Server(("127.0.0.1", 0), Handler)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(STATE / "server.pem", STATE / "server-key.pem")
    server.socket = tls.wrap_socket(server.socket, server_side=True)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def fail(why):
    raise SystemExit(f"system test failed: {why}")


def image_files(path):
    data, at, names = path.read_bytes(), 0, set()
    while data[at:at + 6] == b"070701":
        size, namesize = int(data[at + 54:at + 62], 16), int(data[at + 94:at + 102], 16)
        names.add(data[at + 110:at + 110 + namesize - 1].decode())
        at = (((at + 110 + namesize + 3) & ~3) + size + 3) & ~3
    return names



def main():
    original = ROOT / "build/sel4/qemu"
    if not (original / "initfs.cpio").exists():
        fail("build the test image first (make test-system)")
    cpio = (original / "initfs.cpio").read_bytes()
    files = image_files(original / "initfs.cpio")
    on_home = home_grid.apps(cpio)
    missing = {"wxprobe", "probe", "victim", "settings"} - set(on_home)
    if missing:
        fail(f"the image lacks {sorted(missing)} on Home: build it with make test-system")
    abi = [p.name for p in sorted((ROOT / "targets/qemu/abi-test-apps").iterdir()) if (p / "app.txt").exists()]
    present = [a for a in abi if f"apps/{a}" in files and a in on_home]
    if "apps/abichild" not in files:
        fail("the image lacks /apps/abichild, which the process tests start (is the musl sysroot built?)")
    if "cdyn" in present and "lib/ld-musl-armhf.so.1" not in files:
        fail("cdyn is in the image without /lib/ld-musl-armhf.so.1 (build it with VIOLET_OPEN_LDSO=1)")
    for name in sorted(set(abi) - set(present)):
        print(f"skipping {name}: its toolchain was not available when the image was built", file=sys.stderr)
    with TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        viewer = tmp / "input.py"
        viewer.write_text(home_grid.viewer(SCRIPT, cpio))
        viewer.chmod(0o755)
        # The update the server hands out: this build itself, signed by its key.
        served = tmp / "served"
        served.mkdir()
        image_bytes = ota_bundle.container({name: (original / name).read_bytes() for name in ota_bundle.QEMU_FILES})
        if ota_bundle.image_version(image_bytes) != SYSTEM_VERSION:
            fail(f"the image was not built with VIOLET_SYSTEM_VERSION={SYSTEM_VERSION}")
        signer = sign.config(ROOT, "qemu")
        (served / "good.vota").write_bytes(ota_bundle.bundle(signer, SYSTEM_VERSION, ota_bundle.LAYOUT_QEMU, image_bytes))
        # Deliberately wrong bundles, made without ota_bundle's own version check:
        # an older signed release, and a signed manifest sent with another image.
        (served / "old.vota").write_bytes(ota_bundle.bundle(signer, SYSTEM_VERSION - 1, ota_bundle.LAYOUT_QEMU, image_bytes, check_version=False))
        small = ota_bundle.bundle(signer, SYSTEM_VERSION, ota_bundle.LAYOUT_QEMU, b"\x11" * 8192, check_version=False)
        (served / "forged.vota").write_bytes(small[:512] + b"\x22" * 8192)
        serve_file = tmp / "serve"
        server = https_server(served, serve_file)
        # A fresh disk with misc and two boot slots; slot A holds the image just built.
        image = tmp / "image"
        image.mkdir()
        disk = image / "stopwatch.disk"
        ota_boot.provision(disk, ota_bundle.qemu_image(original))
        run.IMAGE = image
        # QEMU loads the files of the slot that was chosen, not the build's.
        run.machine = partial(run.machine, image=image)
        os.environ.update({
            "ABI_CHECKS": json.dumps(CHECKS), "ABI_PRESENT": json.dumps(present),
            "OTA_TOOLS": str(ROOT / "tools"), "OTA_DISK": str(disk), "OTA_SERVE": str(serve_file),
            "OTA_SYSTEM": str(SYSTEM_VERSION),
            "VIOLET_NET_HOSTS": json.dumps({key: f"127.0.0.1:{server.server_address[1]}" for key in (HOST, f"{HOST}:443")}),
        })

        def boot(stage):
            """One power-on through the model bootloader; the slot that ran."""
            slot = ota_boot.boot(disk, image)
            if slot is None:
                fail(f"boot ({stage}): the chosen slot did not load")
            os.environ["SYSTEM_STAGE"] = stage
            print(f"boot ({stage}) from slot {'AB'[slot]}: {ota_boot.read_control(disk).describe()}")
            status = run.main(str(viewer))
            if status != 0:
                fail(f"{stage} boot from slot {'AB'[slot]} (viewer status {status})")
            return slot

        if boot("probes") != 0:
            fail("the first boot was not from slot A")
        c = ota_boot.read_control(disk)
        if not (c.slot(1)["priority"] == 15 and c.slot(1)["tries"] == ota_boot.ACTIVE_TRIES and not c.slot(1)["successful"]
                and c.slot(0)["successful"]):
            fail(f"B is not staged as a trial: {c.describe()}")
        with disk.open("rb") as f:
            f.seek(ota_boot.QEMU.first[1] * 512)
            want = ota_bundle.pad(ota_bundle.qemu_image(original))
            if f.read(len(want)) != want:
                fail("slot B does not hold the image that was sent")
        if boot("trial") != 1:
            fail("the trial did not boot from B")
        c = ota_boot.read_control(disk)
        if not c.slot(1)["successful"]:
            fail(f"B was not marked successful: {c.describe()}")
        server.shutdown()
    print("System verified on the real kernel: Linux ABI (" + ", ".join(present) + "), W^X and layout randomization on real page "
          "tables, the capability policy and the Shield's side doors through the real dispatch, an older and a forged update refused, "
          "a signed update staged in slot B, booted as a trial and marked successful, the passcode lock kept across the restart")


# --arch aarch64: the same system built for AArch64 (targets/qemu64/stack.py,
# `make qemu64 QEMU64_STACK=1`; not in test-local or CI). One boot, checked
# layer by layer in the order the system comes up: the supervisor creates the
# services, Storage serves /data, the Open Runtime unpacks /apps and draws on
# the 720 x 1600 screen, and Home, a 64-bit program the Open Runtime loads and
# serves through its one dispatcher (runtime/arch/enter64.rs), shows. Then
# Home opens targets/qemu64/test-apps/abi64, which needs the real kernel:
# signal handlers through the A64 gate (a fault's handler returning to the
# faulting write, the SIMD registers intact), the 64-bit siginfo, and
# posix_spawn (clone with CLONE_VM|CLONE_VFORK, execve, pipes, wait4).
# targets/qemu/test_open_apps.py --arch aarch64 then drives Settings, Music
# and a setting kept across a restart, and test_ladybird.py --arch aarch64 the browser.
AARCH64_LAYERS = [
    ("Storage serves /data to the Open Runtime", "[open] /data: "),
    ("the Open Runtime reads the installed apps", "[open] installed apps: "),
    ("the policy reads the app manifests", "[open] policy: "),
    ("the Open Runtime unpacks /apps", "[open] ready: "),
    ("the Open Runtime starts Home, an AArch64 program", "[open] started /apps/home as pid "),
    ("Home is ready", "home: ready"),
    ("Home shows", "home: shown"),
]
AARCH64_ABI = [
    "abi64: usr1 handler ran 2",
    "abi64: siginfo signo 12 code -6",
    "abi64: fault at page+0x8, then wrote 0x5eed, fp 3",
    "abi64: child exit Some(7)",
    'abi64: child said "from the child"',
    "abi64: missing program refused true",
    "abi64 done",
]
AARCH64_SCRIPT = r"""#!/usr/bin/env python3
import json, mmap, os, struct, sys, time
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
screen = mmap.mmap(ram.fileno(), 720 * 1600 * 4, offset=0x79450000 - 0x40000000, prot=mmap.PROT_READ)
LAYERS = json.loads(os.environ["AARCH64_LAYERS"])
ABI = json.loads(os.environ["AARCH64_ABI"])

def text():
    n = struct.unpack_from("<I", log, 4)[0]
    data = bytes(log[8:4096])
    if n <= len(data):
        return data[:n].decode(errors="replace")
    s = n % len(data)
    return (data[s:] + data[:s]).decode(errors="replace")
seen = ""
def everything():
    global seen
    t = text()
    for k in range(min(len(seen), len(t)), 0, -1):
        if seen.endswith(t[:k]):
            seen += t[k:]
            return seen
    seen += t
    return seen
def fail(why):
    print("AArch64 system test failed:", why, "\n--- log ---\n" + everything()[-3000:], file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=30.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.5)

until("Home shows", lambda: "home: shown" in everything(), 120)
at = 0
for what, line in LAYERS:
    found = everything().find(line, at)
    if found < 0:
        fail(f"{what}: no line starting {line!r} after the ones before it")
    print(f"AArch64 layer: {what}: ok", file=sys.stderr)
    at = found
frame = screen[:]
drawn = sum(1 for i in range(0, len(frame), 4 * 101) if frame[i:i + 3] != b"\0\0\0")
if drawn < (len(frame) // (4 * 101)) // 2:
    fail(f"the screen is not drawn ({drawn} of {len(frame) // (4 * 101)} sampled pixels are not black)")
print("AArch64 layer: Home draws the 720 x 1600 screen: ok", file=sys.stderr)
# The timer this image was built with is the one the Open Runtime runs with.
polling = "no timer: polling at priority" in everything()
if polling != (os.environ["AARCH64_USER_TIMER"] == "0"):
    fail("the Open Runtime's timer is not what differences.json says (user timer %s, polling %s)" % (os.environ["AARCH64_USER_TIMER"], polling))
until("the Sentinel answers", lambda: "[open] shield: mode enforce" in everything(), 30)
print("AArch64 layer: the Sentinel answers (Violet Shield): ok", file=sys.stderr)
time.sleep(1)
start = len(everything())
open_icon("abi64")
until("abi64 finishes", lambda: "abi64 done" in everything()[start:], 60)
out = everything()[start:]
for line in ABI:
    if line not in out:
        fail(f"abi64 did not print {line!r}")
until("abi64 exits", lambda: "exited with status 0" in everything()[start:], 15)
print("AArch64 layer: signals (handler, siginfo, a fault's handler) and posix_spawn in a 64-bit program: ok", file=sys.stderr)
notes = sorted({l.strip() for l in everything().splitlines() if "unsupported AArch64 system call" in l})
if notes:
    print("AArch64: note: " + "; ".join(notes), file=sys.stderr)
"""


def aarch64_layers():
    original = run.IMAGES["aarch64"]
    if not (original / "qemu64.json").exists():
        fail(f"build the AArch64 system first: make qemu64-image QEMU64_STACK=1 ({original})")
    # What this image gives up against AArch32 (targets/qemu64/stack.py
    # differences.json): printed as layers that are absent, never as passes,
    # and checked against the boot below.
    known = original / "differences.json"
    if not known.exists():
        fail(f"the AArch64 image has no differences.json: rebuild it with targets/qemu64/stack.py ({original})")
    differences = json.loads(known.read_text())
    if differences.get("verified_boot") is not False or not differences.get("differences"):
        fail(f"{known} does not record the AArch64 image's verified boot as absent: rebuild it")
    for d in differences["differences"]:
        print(f"AArch64 layer ABSENT (security difference from AArch32): {d}", file=sys.stderr)
    os.environ["AARCH64_USER_TIMER"] = "1" if differences["user_timer"] else "0"
    cpio = (original / "initfs.cpio").read_bytes()
    if "abi64" not in home_grid.apps(cpio):
        fail("the AArch64 image has no abi64 (targets/qemu64/test-apps): rebuild it with targets/qemu64/stack.py")
    with TemporaryDirectory() as tmp:
        image = Path(tmp) / "image"
        image.mkdir()
        for name in ("qemu64.bin", "qemu64.json", "initfs.cpio"):
            (image / name).symlink_to(original / name)
        os.environ["VIOLET_ARCH"] = "aarch64"
        run.IMAGES["aarch64"] = image
        os.environ["AARCH64_LAYERS"] = json.dumps(AARCH64_LAYERS)
        os.environ["AARCH64_ABI"] = json.dumps(AARCH64_ABI)
        viewer = Path(tmp) / "input.py"
        viewer.write_text(home_grid.viewer(AARCH64_SCRIPT, cpio))
        viewer.chmod(0o755)
        if run.main(str(viewer)) != 0:
            fail("the AArch64 system test failed (above)")
    print("AArch64 system checked: kernel, supervisor and services, /data, the Open Runtime with its policy and "
          "Sentinel, the 720 x 1600 screen, Home as a 64-bit program, and a 64-bit program's signal handlers and posix_spawn. "
          "Absent on AArch64: " + "; ".join(differences["differences"]))


if __name__ == "__main__":
    if sys.argv[1:] == ["--prepare"]:
        prepare()
    elif sys.argv[1:] == ["--arch", "aarch64"]:
        aarch64_layers()
    else:
        main()
