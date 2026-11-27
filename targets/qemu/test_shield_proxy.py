"""Exercise the paired Agent flow cross-check with real SOCKS traffic.

Build the QEMU image and Mac app with Shield models first. Run on macOS:
  python3 targets/qemu/test_shield_proxy.py Mac
  python3 targets/qemu/test_shield_proxy.py iPhone --ios-port 19823

For the iPhone case, run Violet Relay in an iOS simulator with -upstream any
and -port 19823. Set VIOLET_TEST_RELAY_PASSWORD to its test pairing secret.
Both upstreams are loopback-only. Each run uses a fresh guest disk and Mac profile.
"""
import argparse
import functools
import json
import mmap
import os
from pathlib import Path
import re
import socket
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path[:0] = [str(ROOT / "tools"), str(Path(__file__).resolve().parent)]
import home_grid
import net_relay
import run


def wait_for(label, check, seconds=60):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        result = check()
        if result:
            return result
        time.sleep(0.15)
    raise RuntimeError(f"Timed out waiting for {label}")


def proxy_only(port, password, target_port):
    with socket.create_connection(("127.0.0.1", port), timeout=5) as conn, conn.makefile("rb") as reply:
        conn.sendall(bytes([5, 1, 2]))
        assert reply.read(2) == bytes([5, 2])
        password = password.encode()
        conn.sendall(bytes([1, 6]) + b"violet" + bytes([len(password)]) + password)
        assert reply.read(2) == bytes([1, 0])
        conn.sendall(bytes([5, 1, 0, 1, 127, 0, 0, 1]) + target_port.to_bytes(2, "big"))
        assert reply.read(2) == bytes([5, 0])
        conn.sendall(b"proxy-only test")


def viewer(ram):
    config = json.loads(os.environ["SHIELD_PROXY_TEST"])
    base = Path(config["directory"])
    support = Path.home() / "Library/Application Support" / ("Violet Agent " + config["profile"])
    icons = json.loads(os.environ["HOME_ICONS"])

    def tap(x, y):
        os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
        time.sleep(0.15)
        os.write(1, bytes(6))
        time.sleep(0.35)

    def rows():
        log = support / "shield-verdicts.jsonl"
        return [json.loads(line) for line in log.open() if line.endswith("\n")] if log.exists() else []

    def connected():
        status = support / "shield-status.json"
        if status.exists():
            data = json.loads(status.read_text())
            return data["link"] == "Mac Agent" and data["flow_observer"] == config["source"]

    with open(ram, "rb") as f, mmap.mmap(f.fileno(), 4096, access=mmap.ACCESS_READ, offset=0x9220000) as log:
        def guest_log():
            n = int.from_bytes(log[4:8], "little")
            ring = log[8:]
            pos = n % len(ring)
            return (ring[:n] if n <= len(ring) else ring[pos:] + ring[:pos]).decode(errors="replace")

        wait_for("Home", lambda: "home: ready" in guest_log())
        time.sleep(2)
        tap(*icons["agent"])
        code = wait_for("pairing invitation", lambda: re.search(r"agent: pairing code (\S+)", guest_log()))[1]
        env = dict(os.environ, VIOLET_REPO=str(base), VIOLET_AGENT_PROFILE=config["profile"])
        mac = subprocess.Popen([config["mac"], "-ApplePersistenceIgnoreState", "YES", "--no-speech", "--pair", code],
                               env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            wait_for("paired " + config["source"] + " proxy", connected, 180)
            time.sleep(4)
            tap(100, 60)
            time.sleep(1)
            tap(*icons["flowcheck"])
            match = f"127.0.0.1:{config['matched']} SOCKS5_SEEN=1"
            wait_for("corroborated guest socket", lambda: any(r["app"] == "flowcheck" and match in r["window"] for r in rows()))
            if config["source"] == "Mac":
                relay = json.loads((base / "build/local-companion/connection.json").read_text())
                port, password = relay["socksPort"], relay["password"]
            else:
                port, password = config["ios_port"], os.environ["VIOLET_TEST_RELAY_PASSWORD"]
            proxy_only(port, password, config["unmatched"])
            def reported(port):
                return any(r["tier"] == "relay" and f"127.0.0.1:{port}" in r["hosts"] for r in rows())
            wait_for("proxy-only verdict", lambda: reported(config["unmatched"]))
            assert not reported(config["matched"])
        finally:
            mac.terminate()
            mac.wait(timeout=10)


class Sink(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(5)
        self.request.recv(64)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", choices=("Mac", "iPhone"))
    parser.add_argument("--ios-port", type=int, default=19823)
    parser.add_argument("--mac", type=Path, default=ROOT / "build/mac-agent/Build/Products/Debug/Violet Agent.app/Contents/MacOS/Violet Agent")
    args = parser.parse_args()
    if args.source == "iPhone" and not os.environ.get("VIOLET_TEST_RELAY_PASSWORD"):
        parser.error("Set VIOLET_TEST_RELAY_PASSWORD for the simulator relay")
    with tempfile.TemporaryDirectory(prefix="shield-proxy-") as directory, \
            socketserver.ThreadingTCPServer(("127.0.0.1", 0), Sink) as matched, \
            socketserver.ThreadingTCPServer(("127.0.0.1", 0), Sink) as unmatched:
        base = Path(directory)
        for listener in (matched, unmatched):
            threading.Thread(target=listener.serve_forever, daemon=True).start()
        repo_build = base / "build/shield"
        repo_build.mkdir(parents=True)
        (repo_build / "companion-signing.pem").symlink_to(ROOT / "build/shield/companion-signing.pem")
        (base / "tools").symlink_to(ROOT / "tools")
        app = base / "apps/flowcheck"
        app.mkdir(parents=True)
        (app / "app.txt").write_text("Flowcheck\n")
        (app / "main.rs").write_text(
            'use std::{io::Write, net::TcpStream, thread, time::Duration};\nfn main() {\n'
            f'    let mut stream = TcpStream::connect(("127.0.0.1", {matched.server_address[1]})).unwrap();\n'
            '    stream.write_all(b"flow check").unwrap();\n    thread::sleep(Duration::from_secs(45));\n}\n'
        )
        image = base / "image"
        image.mkdir()
        for name in ("entry.bin", "handoff.dtb", "zImage"):
            (image / name).symlink_to(ROOT / "build/sel4/qemu" / name)
        # Chariot prints the bound port on startup. Keep the test pairing local.
        relay = subprocess.Popen([sys.executable, str(ROOT / "tools/chariot-relay/relay.py"), "--host", "127.0.0.1",
                                  "--port", "0"], stdout=subprocess.PIPE, text=True)
        try:
            line = relay.stdout.readline()
            port = int(line.strip().rsplit(":", 1)[1])
            env = dict(os.environ, VIOLET_AGENT_RELAY=f"http://127.0.0.1:{port}", VIOLET_AGENT_STUN="off",
                       VIOLET_OPEN_EXTRA_APPS=str(app.parent), VIOLET_OPEN_SKIP="ladybird,bitchat,music", VIOLET_OPEN_NO_PYTHON="1")
            subprocess.run([sys.executable, str(ROOT / "tools/build_initfs.py"), str(image / "initfs.cpio")], env=env, check=True)
            run.IMAGE = image
            run.machine = functools.partial(run.machine, image)
            net_relay.LOCAL_COMPANION = base / "build/local-companion/connection.json"
            for key in ("VIOLET_NET_HOSTS", "VIOLET_NET_SOCKS", "VIOLET_NET_SOCKS_SCOPE"):
                os.environ.pop(key, None)
            os.environ["VIOLET_NET_UDP"] = "0"
            if args.source == "iPhone":
                os.environ.update(VIOLET_NET_SOCKS=f"127.0.0.1:{args.ios_port}",
                                  VIOLET_NET_SOCKS_PASS=os.environ["VIOLET_TEST_RELAY_PASSWORD"])
            names = home_grid.apps((image / "initfs.cpio").read_bytes())
            os.environ["HOME_ICONS"] = json.dumps({n: home_grid.icon(i, names) for i, n in enumerate(names)})
            os.environ["SHIELD_PROXY_TEST"] = json.dumps(dict(directory=directory, source=args.source, mac=str(args.mac.resolve()),
                profile="flow-" + uuid.uuid4().hex[:12], matched=matched.server_address[1],
                unmatched=unmatched.server_address[1], ios_port=args.ios_port))
            launcher = base / "viewer"
            launcher.write_text(f"#!{sys.executable}\nimport runpy\nrunpy.run_path({str(Path(__file__).resolve())!r}, run_name='__main__')\n")
            launcher.chmod(0o755)
            if run.main(str(launcher)):
                raise SystemExit("Proxy cross-check failed")
            print(args.source, "proxy verified in both directions")
        finally:
            relay.terminate()
            relay.wait()
            for listener in (matched, unmatched):
                listener.shutdown()


if __name__ == "__main__":
    if os.environ.get("SHIELD_PROXY_TEST"):
        viewer(sys.argv[1])
    else:
        main()
