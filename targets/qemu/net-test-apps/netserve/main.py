"""Listening sockets and IPv6, for targets/qemu/test_net.py: servers on the
phone's loopback (in this app and through asyncio), IPv6 sockets on the
addresses the runtime carries, the resolver's AAAA answers, then a TCP
listener and a bound UDP socket the Mac reaches through the emulator's relay.
Each check prints one "netserve: name=value" line."""
import asyncio, http.server, socket, threading, urllib.request

LAN_PORT = 8090


def out(name, value):
    print(f"netserve: {name}={value}", flush=True)


def check(name, fn):
    try:
        out(name, fn())
    except OSError as e:
        out(name, f"errno{e.errno}" if e.errno else type(e).__name__)
    except Exception as e:
        out(name, type(e).__name__)


class Page(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"served-on-the-phone"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def http_loopback():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Page)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with urllib.request.urlopen(f"http://127.0.0.1:{server.server_address[1]}/", timeout=10) as reply:
        body = reply.read().decode()
    server.shutdown()
    server.server_close()
    return body


def refused():
    probe = socket.socket()
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
    probe.close()
    socket.create_connection(("127.0.0.1", port), timeout=5)
    return "connected"


def v6_loopback():
    server = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
    server.bind(("::1", 0))
    server.listen(4)
    client = socket.create_connection(("::1", server.getsockname()[1]), timeout=5)
    conn, peer = server.accept()
    client.sendall(b"six")
    return f"{server.getsockname()[0]},{peer[0]},{conn.recv(16).decode()}"


def dual_stack():
    server = socket.create_server(("127.0.0.1", 0), family=socket.AF_INET)
    port = server.getsockname()[1]
    mapped = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
    mapped.settimeout(5)
    mapped.connect(("::ffff:127.0.0.1", port))
    conn, peer = server.accept()
    mapped.sendall(b"mapped")
    return f"{peer[0]},{conn.recv(16).decode()},{mapped.getpeername()[0]}"


def v6_wildcard():
    server = socket.create_server(("::", 0), family=socket.AF_INET6, dualstack_ipv6=True)
    client = socket.create_connection(("127.0.0.1", server.getsockname()[1]), timeout=5)
    conn, peer = server.accept()
    client.sendall(b"dual")
    return f"{server.getsockname()[0]},{peer[0]},{conn.recv(16).decode()}"


def families():
    found = {ai[0].name for ai in socket.getaddrinfo("peer.test", 80, type=socket.SOCK_STREAM)}
    return ",".join(sorted(found))


def v6_only_lookup():
    try:
        return len(socket.getaddrinfo("peer.test", 80, socket.AF_INET6, socket.SOCK_STREAM))
    except socket.gaierror:
        return "none"


def global_v6():
    s = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
    s.settimeout(5)
    s.connect(("2001:db8::1", 80))
    return "connected"


def pair():
    a, b = socket.socketpair()
    a.sendall(b"pair")
    return b.recv(8).decode()


def aio():
    async def echo(reader, writer):
        writer.write((await reader.read(100)).upper())
        await writer.drain()
        writer.close()

    async def go():
        server = await asyncio.start_server(echo, "127.0.0.1", 0)
        reader, writer = await asyncio.open_connection("127.0.0.1", server.sockets[0].getsockname()[1])
        writer.write(b"async")
        await writer.drain()
        got = await reader.read(100)
        writer.close()
        server.close()
        return got.decode()

    return asyncio.run(asyncio.wait_for(go(), 20))


def udp_loopback():
    r = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    r.bind(("127.0.0.1", 0))
    r.settimeout(5)
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.sendto(b"dgram", r.getsockname())
    data, source = r.recvfrom(64)
    return f"{data.decode()},{source[0]},{source[1] == s.getsockname()[1]}"


def lan_tcp():
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("0.0.0.0", LAN_PORT))
    server.listen(2)
    server.settimeout(90)
    out("listening", server.getsockname()[1])
    conn, _ = server.accept()
    conn.settimeout(10)
    data = conn.recv(64)
    conn.sendall(b"phone:" + data)
    conn.close()
    return data.decode()


def lan_udp():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", LAN_PORT))
    s.settimeout(60)
    out("udp_bound", s.getsockname()[1])
    data, source = s.recvfrom(64)
    s.sendto(b"phone:" + data, source)
    return data.decode()


CHECKS = [("http", http_loopback), ("refused", refused), ("v6", v6_loopback), ("mapped", dual_stack),
          ("dual", v6_wildcard), ("families", families), ("v6lookup", v6_only_lookup), ("global6", global_v6),
          ("pair", pair), ("asyncio", aio), ("udp", udp_loopback), ("lan_tcp", lan_tcp), ("lan_udp", lan_udp)]
for name, fn in CHECKS:
    check(name, fn)
out("done", 1)
