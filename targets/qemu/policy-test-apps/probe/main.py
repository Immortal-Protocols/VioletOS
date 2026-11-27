"""The capability policy from inside an app (targets/qemu/test_policy.py).
No manifest, so it may use only its own /data, the screen, touch and the
unprivileged /dev/violet/sys commands. Each check prints a line; any
"policy: FAIL" fails the test."""
import errno
import os
import socket

failures = []
checks = 0


def expect(what, code, fn):
    """`code` may be a tuple: a write to /apps, /bin or / may be refused by
    Violet Shield (EACCES) before the policy (EROFS) sees it."""
    global checks
    checks += 1
    try:
        fn()
        got = 0
    except OSError as e:
        got = e.errno
    if got not in (code if isinstance(code, tuple) else (code,)):
        failures.append(what)
        print(f"policy: FAIL {what}: errno {got}, expected {code}", flush=True)


def write(path):
    with open(path, "w") as f:
        f.write("x")


def read(path):
    with open(path, "rb") as f:
        f.read()


def sys_command(line):
    fd = os.open("/dev/violet/sys", os.O_RDWR)
    try:
        os.write(fd, (line + "\n").encode())
        return os.read(fd, 4096).decode().strip()
    finally:
        os.close(fd)


# Its own /data.
expect("write own file", 0, lambda: write("/data/probe"))
expect("write own dotted file", 0, lambda: write("/data/probe.state"))
expect("read own file", 0, lambda: read("/data/probe"))
# Another app's files do not exist, and cannot be made or removed.
for path in ["/data/victim", "/data/victim.keys", "/data/networks", "/data/agent", "/data/.settings", "/data/.grants", "/data/.shield-mode"]:
    expect(f"read {path}", errno.ENOENT, lambda p=path: read(p))
    expect(f"stat {path}", errno.ENOENT, lambda p=path: os.stat(p))
# Writes look missing too (ENOENT), unless Violet Shield refuses first (EACCES).
HIDDEN = (errno.ENOENT, errno.EACCES)
expect("write another app's file", HIDDEN, lambda: write("/data/victim"))
expect("make a file under another name", HIDDEN, lambda: write("/data/probex"))
expect("remove another app's file", HIDDEN, lambda: os.unlink("/data/victim"))
expect("rename into another app's name", HIDDEN, lambda: os.rename("/data/probe", "/data/victim"))
# O_CREAT|O_RDONLY is a read of a file that exists and a write of one that
# does not: the answer must not tell which.
def errno_of(fn):
    try:
        fn()
        return 0
    except OSError as e:
        return e.errno
for there, missing in [("/data/victim", "/data/victimq"), ("/data/.grants", "/data/.nothing")]:
    checks += 1
    a, b = (errno_of(lambda p=p: os.close(os.open(p, os.O_CREAT | os.O_RDONLY))) for p in (there, missing))
    if a != b or a == 0:
        failures.append(f"existence of {there}")
        print(f"policy: FAIL O_CREAT open tells {there} ({a}) from {missing} ({b})", flush=True)
# /tmp is shared, but its dot names are only for the image apps that name them.
expect("write a /tmp file", 0, lambda: write("/tmp/probe-scratch"))
for path in ["/tmp/.ota-chunk", "/tmp/.locomo.make"]:
    expect(f"write {path}", errno.ENOENT, lambda p=path: write(p))
    expect(f"read {path}", errno.ENOENT, lambda p=path: read(p))
expect("climb out of its own folder", errno.ENOENT, lambda: read("/data/probe.d/../victim"))
listed = sorted(os.listdir("/data"))
checks += 1
if any(not (n == "probe" or n.startswith("probe.")) for n in listed):
    failures.append("listing /data")
    print(f"policy: FAIL /data lists {listed}", flush=True)
# /apps, /bin and /lib are the image's.
expect("overwrite Home", (errno.EROFS, errno.EACCES), lambda: write("/apps/home"))
expect("rename itself to Settings", (errno.EROFS, errno.EACCES), lambda: os.rename("/apps/probe", "/apps/settings"))
expect("stage an install", (errno.EROFS, errno.EACCES), lambda: write("/apps/.agent-install"))
expect("remove the manifests", (errno.EROFS, errno.EACCES), lambda: os.unlink("/apps/.manifests"))
expect("add to /bin", (errno.EROFS, errno.EACCES), lambda: os.mkdir("/bin/x"))
expect("add to /", (errno.EROFS, errno.EACCES), lambda: write("/x"))
expect("read Home's program", 0, lambda: read("/apps/home"))
# Devices that take a capability, and sockets.
for node in ["mic", "camera", "hci", "audio"]:
    expect(f"open /dev/violet/{node}", errno.EACCES, lambda n=node: os.close(os.open(f"/dev/violet/{n}", os.O_RDWR)))
expect("open /dev/violet/nothing", errno.ENOENT, lambda: os.close(os.open("/dev/violet/nothing", os.O_RDWR)))
expect("make a TCP socket", errno.EACCES, lambda: socket.socket(socket.AF_INET, socket.SOCK_STREAM).close())
expect("make a UDP socket", errno.EACCES, lambda: socket.socket(socket.AF_INET, socket.SOCK_DGRAM).close())
# /dev/violet/sys: the system's powers are not an app's.
for line in ["open /apps/victim", "launch /apps/victim", "install probe Probe", "uninstall victim", "wifi join 0 00", "relay on 123456789abc",
             "mic record", "sound chime", "shield-mode monitor", "shield-agent pull", "update info"]:
    checks += 1
    answer = sys_command(line)
    # "error not allowed (needs ...)" from the policy, or the trusted UI's own refusal first.
    if not answer.startswith("error"):
        failures.append(line)
        print(f"policy: FAIL '{line}' answered {answer!r}", flush=True)
# Asking ahead for a power its manifest does not declare raises no prompt.
for line, want in [("set probecolor violet", "ok"), ("get probecolor", "violet"), ("consent net", "deny"), ("consent hci", "deny")]:
    checks += 1
    answer = sys_command(line)
    if answer != want:
        failures.append(line)
        print(f"policy: FAIL '{line}' answered {answer!r}, expected {want!r}", flush=True)
print(f"policy: {'ok' if not failures else 'FAILED'} {checks} checks", flush=True)
sys_command("home")
