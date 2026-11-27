"""An app that takes all the network it can, for targets/qemu/test_net.py:
it asks the resolver for more names than it keeps, uses an address whose name
is gone, opens sockets until it is refused, leaves one connect hanging and goes
to the background, still holding everything."""
import errno, select, socket, time


def out(*words):
    print("nethog:", *words, flush=True)


def connect_error(address, port):
    s = socket.socket()
    s.settimeout(5)
    try:
        s.connect((address, port))
        return 0
    except OSError as e:
        return e.errno
    finally:
        s.close()


# More names than one app's share of the resolver's table (256).
first = socket.gethostbyname("n0.hog.test")
addresses = {first}
for i in range(1, 300):
    addresses.add(socket.gethostbyname(f"n{i}.hog.test"))
inside = all(a.startswith(("198.18.", "198.19.")) for a in addresses)
out(f"names={len(addresses)} inside={inside}")
# The first name was dropped to make room: its address must go nowhere, as
# must a stand-in address nobody was given, and a name that is not one.
try:
    socket.gethostbyname("not a name.test")
    bad = "resolved"
except OSError:
    bad = "refused"
out(f"stale={connect_error(first, 80)} unknown={connect_error('198.18.200.200', 80)} badname={bad}")

# Sockets until the runtime says no.
held, refused = [], 0
for _ in range(40):
    s = socket.socket()
    s.settimeout(10)
    try:
        s.connect(("sink.test", 80))
    except OSError as e:
        refused = e.errno
        s.close()
        break
    held.append(s)
out(f"held={len(held)} errno={refused}")

# One of them becomes a connect to a host that does not answer.
held.pop().close()
time.sleep(0.5)
hole = socket.socket()
hole.setblocking(False)
started = hole.connect_ex(("hole.test", 80))
out(f"hanging started={started == errno.EINPROGRESS}")
with open("/dev/violet/sys", "wb", buffering=0) as system:
    system.write(b"home\n")
began = time.monotonic()
# poll, not select: the Open Runtime has no select system call.
waiter = select.poll()
waiter.register(hole, select.POLLOUT)
waiter.poll(60000)
out(f"hole={hole.getsockopt(socket.SOL_SOCKET, socket.SO_ERROR)} after={time.monotonic() - began:.0f}s still_held={len(held)}")
