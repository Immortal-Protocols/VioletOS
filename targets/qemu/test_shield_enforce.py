"""Violet Shield enforcing on its own, in QEMU (local: `make test-shield-enforce`).

    VIOLET_SHIELD_MODE=strict make qemu-image
    build/shield/venv/bin/python targets/qemu/test_shield_enforce.py

Needs an image whose Shield starts in strict mode, because the demo apps
(apps/shield-demo) are built into the test image and image apps are only
contained in strict mode. The companion simulator runs without its model, as
the SOCKS5 relay that maps the demo's .test hosts to local servers; it signs
nothing but warnings, so every refusal, quarantine and freeze this test
looks for was decided by the phone (shield ids below 16).

  1. notes (ordinary) runs to the end and is not touched.
  2. photosync reads notes' data, goes to the background and starts its
     4 MiB upload: the phone quarantines it and most of the upload never leaves.
  3. themes tries to overwrite /apps/home: the write is refused (shield 1),
     and so are a write and a truncate through a read-only descriptor and a
     read-only open with O_TRUNC.
  4. Home still answers.
"""
from __future__ import annotations

import functools
import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path
from tempfile import TemporaryDirectory

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_shield as ts  # noqa: E402

ROOT = ts.ROOT
# The scripted viewer's helpers from test_shield.py, then this test's steps.
HELPERS = ts.SCRIPT.split('until("Open Runtime ready"')[0]
STEPS = r'''
until("Open Runtime ready", lambda: "[open] ready" in everything(), 90)
until("the Shield reports its mode", lambda: "shield: mode " in everything(), 30)
if "shield: mode strict" not in everything():
    fail("this image does not start in strict mode: build it with VIOLET_SHIELD_MODE=strict make qemu-image")
caption("notes: an ordinary app; nothing happens to it")
start("notes")
until("notes runs", lambda: "notes done" in everything(), 90)
until("Home after notes", on_home, 15)

caption("photosync: reads notes' secrets, hides, uploads; the phone quarantines it by itself")
start("photosync")
until("photosync hides", lambda: "going to the background" in everything(), 90)
until("Home", on_home, 15)
until("photosync quarantined", lambda: ": quarantined pid" in everything(), 120)
hold(6)

caption("themes: tries to overwrite Home's program; the built-in rule refuses the write")
start("themes")
until("themes write refused", lambda: "shield 1: denied write /apps/home" in everything(), 60)
# A read-only descriptor cannot write or truncate, and O_RDONLY|O_TRUNC is a write
# (the scorer may freeze themes for 30 s first: it carries on when thawed).
until("themes tries the side doors", lambda: "themes: side doors:" in everything(), 120)
if "themes: side doors: write -1 truncate -1 open-trunc -1" not in everything():
    fail("a read-only descriptor or a truncating open changed /apps/home")
if "themes: hook written" in everything():
    fail("themes overwrote /apps/home")
hold(3)
back_home()
hold(2)
log = everything()
for bad in ("quarantined pid 100 ", "froze pid 100 "):
    if bad in log:
        fail("Home (pid 100) was contained")
print("shield enforce scripted run complete", file=sys.stderr)
'''


def start_relay(host_map, tmp: Path):
    """The companion simulator without its model: relay and host map only."""
    port = ts.free_port()
    status = tmp / "shield-status.json"
    python = ROOT / "build/shield/venv/bin/python"
    proc = subprocess.Popen([str(python if python.exists() else sys.executable), str(ROOT / "tools/shield-trainer/companion_sim.py"),
                             "--port", str(port), "--status", str(status), "--host-map", json.dumps(host_map),
                             "--record", str(tmp / "records.jsonl")])
    deadline = time.time() + 60
    while time.time() < deadline:
        try:
            socket.create_connection(("127.0.0.1", port), timeout=1).close()
            return port, status, proc
        except OSError:
            time.sleep(0.5)
    raise SystemExit("companion_sim did not start")


def main():
    original = ROOT / "build/sel4/qemu"
    if not (original / "zImage").exists():
        raise SystemExit("Build the QEMU image first (VIOLET_SHIELD_MODE=strict make qemu-image).")
    sink, got = ts.sink_server()
    web = ts.web_server()
    host_map = {"drop.c2-exfil.test": f"127.0.0.1:{sink.server_address[1]}",
                "api.weather.test": f"127.0.0.1:{web.server_address[1]}", "sync.notes.test": f"127.0.0.1:{web.server_address[1]}"}
    with TemporaryDirectory(prefix="vs-", dir=os.environ.get("TMPDIR", "/tmp")) as tmp:
        tmp = Path(tmp)
        image = tmp / "image"
        image.mkdir()
        for name in ("zImage", "handoff.dtb", "entry.bin"):
            (image / name).symlink_to(original / name)
        ts.build_image(image / "initfs.cpio")
        ts.run.IMAGE = image
        ts.run.machine = functools.partial(ts.run.machine, image)
        port, status, helper = start_relay(host_map, tmp)
        viewer = tmp / "viewer.py"
        script = (HELPERS + STEPS).replace("#!PYTHON", "#!" + sys.executable)
        viewer.write_text(ts.home_grid.viewer(script, (image / "initfs.cpio").read_bytes()))
        viewer.chmod(0o755)
        os.environ.update(VIOLET_NET_SOCKS=f"127.0.0.1:{port}", VIOLET_NET_SOCKS_PASS="violet-dev", SHIELD_APPS=json.dumps(ts.APPS),
                          SHIELD_STATUS=str(status), SHIELD_SOCKS_PORT=str(port), SHIELD_PASS="violet-dev")
        try:
            code = ts.run.main(str(viewer))
        finally:
            helper.terminate()
        # Keep the complete records of this run: a trace for the replay harness.
        records = tmp / "records.jsonl"
        if records.exists():
            out = ROOT / "build/shield/qemu-traces"
            out.mkdir(parents=True, exist_ok=True)
            lines = [json.loads(l) for l in records.read_text().splitlines() if l.strip()]
            (out / "enforce-last.log").write_text("".join(
                "\t".join(str(r[k]) for k in ("t_ms", "pid", "app", "sha8", "kind", "flags", "idle_s", "detail", "target")) + "\n" for r in lines))
    problems = []
    if code != 0:
        problems.append(f"scripted run exited with {code}")
    if got["bytes"] >= 2 * 1024 * 1024:
        problems.append(f"photosync's upload was not cut: {got['bytes']} bytes arrived")
    print(f"sink received {got['bytes'] // 1024} KiB of photosync's 4096 KiB", file=sys.stderr)
    if problems:
        print("Violet Shield enforcement test FAILED:\n  " + "\n  ".join(problems), file=sys.stderr)
        sys.exit(1)
    print("Violet Shield enforced on the device", file=sys.stderr)


if __name__ == "__main__":
    main()
