"""Full-system update (OTA) end to end in QEMU, with tools/ota_boot.py as the bootloader.

    make test-ota    (local; builds a test image first, then the normal one again)

QEMU has no bootloader, so this test is one: the data disk gets misc and two
64 MiB boot slots after its first 16 MiB (tools/ota_boot.py), slot A holds the
image just built (system version 2) and B is empty. Before each QEMU start the
test picks a slot from bootloader_control as an A/B bootloader does, spends a
try if the slot is not proven, and starts QEMU with that slot's files. A
restart is QEMU stopping and this loop going round again.

Boot 1 (A): Settings › Updates fetches bundles over HTTPS from a local server
  (TLS by a test CA that only this image trusts) and is refused an older
  version and a bundle whose image is not the one its manifest signed, both
  before anything is switched; then installs a good version 2 into B (the
  same build: a bundle's version must be its image's own VIOLET_SYSTEM_VERSION,
  tools/ota_bundle.py, and the floor allows an equal version).
Boot 2 (B, a trial): the system runs healthy, the supervisor asks Storage to
  mark B successful; the test reads that from the disk.
Boot 3 (B): installs a validly signed version 2 whose kernel image is
  zeroed into A (a release that does not start).
Boots 4-9 (A): each attempt hangs and spends one of A's six tries.
Boot 10: the bootloader falls back to B, which starts, and A is unbootable.

The image must have been built with VIOLET_SYSTEM_VERSION=2,
VIOLET_UPDATE_URL=https://updates.violet.test/latest.vota and
VIOLET_UPDATE_TEST_ROOT (the test CA from --prepare), as `make test-ota` does.
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
URL = f"https://{HOST}/latest.vota"
SYSTEM_VERSION = 2

SCRIPT = r'''#!/usr/bin/env python3
import json, mmap, os, struct, sys, time
sys.path.insert(0, os.environ["OTA_TOOLS"])
import ota_boot
ram = open(sys.argv[1], "r+b")
log = mmap.mmap(ram.fileno(), 0x10000, offset=0x49220000 - 0x40000000)
ICONS = json.loads(os.environ["OPEN_ICONS"])
STAGE = os.environ["OTA_STAGE"]
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
    global seen
    t = text()
    for k in range(min(len(seen), len(t)), 0, -1):
        if seen.endswith(t[:k]):
            seen += t[k:]
            return seen
    seen += t
    return seen
def fail(why):
    print("ota test failed:", why, "\n--- log ---\n" + everything()[-3000:], file=sys.stderr)
    sys.exit(1)
def until(what, check, timeout=15.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.1)
    fail(what)
def tap(x, y):
    os.write(1, bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]))
    time.sleep(.15); os.write(1, bytes(6)); time.sleep(.5)
def serve(name):
    open(SERVE, "w").write(name)
def control():
    return ota_boot.read_control(DISK)

if STAGE == "attempt":
    # A slot that should not start: wait well past the health period and
    # report whether anything marked it.
    time.sleep(float(os.environ.get("OTA_ATTEMPT_SECONDS", "45")))
    sys.exit(0)
until("Home starts", lambda: "home: ready" in everything(), 120)
if STAGE == "healthy":
    slot = control().suffix()
    until("the supervisor marks the slot successful", lambda: control().slot(slot)["successful"], 90)
    print(f"ota: slot {'AB'[slot]} marked successful after a healthy boot", file=sys.stderr)
    sys.exit(0)
time.sleep(2)
tap(*ICONS["settings"])
until("Settings starts", lambda: "settings: ready" in everything(), 60)
time.sleep(1)
tap(360, 250 + 7 * 124 + 54)  # Updates, the menu's eighth row
until("Updates opens", lambda: "settings: updates, system=Some(%s)" % os.environ["OTA_SYSTEM"] in everything(), 30)
def install(name, expect, timeout):
    before = everything().count("settings: update ")
    serve(name)
    tap(360, 955)  # Download and install
    until(f"{name}: {expect}", lambda: everything().count("settings: update ") > before and expect in everything()[-4000:], timeout)
    print(f"ota: {name}: {expect}", file=sys.stderr)
if STAGE == "install":
    install("old.vota", "older than this system", 120)
    install("forged.vota", "does not match its signature", 300)
    install("good.vota", "update staged version=2", 1800)
elif STAGE == "broken":
    install("broken.vota", "update staged version=2", 1800)
time.sleep(2)
'''


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


def bundles(directory, image_dir):
    """The four bundles the server hands out, signed by this build's key."""
    signer = sign.config(ROOT, "qemu")
    files = {name: (image_dir / name).read_bytes() for name in ota_bundle.QEMU_FILES}
    good = ota_bundle.container(files)
    if ota_bundle.image_version(good) != SYSTEM_VERSION:
        fail(f"the image was not built with VIOLET_SYSTEM_VERSION={SYSTEM_VERSION}")
    out = {
        # The build's own version: ota_bundle refuses any other for this image.
        "good.vota": ota_bundle.bundle(signer, SYSTEM_VERSION, ota_bundle.LAYOUT_QEMU, good),
        # Deliberately wrong bundles, made without that check.
        "old.vota": ota_bundle.bundle(signer, SYSTEM_VERSION - 1, ota_bundle.LAYOUT_QEMU, good, check_version=False),
        "broken.vota": ota_bundle.bundle(signer, SYSTEM_VERSION, ota_bundle.LAYOUT_QEMU,
                                         ota_bundle.container({**files, "zImage": bytes(len(files["zImage"]))}),
                                         check_version=False),
    }
    # A signed manifest for one small image, sent with another of the same size.
    signed = ota_bundle.bundle(signer, SYSTEM_VERSION, ota_bundle.LAYOUT_QEMU, b"\x11" * 8192, check_version=False)
    out["forged.vota"] = signed[:512] + b"\x22" * 8192
    for name, data in out.items():
        (directory / name).write_bytes(data)


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
    raise SystemExit(f"ota test failed: {why}")


def main():
    original = ROOT / "build/sel4/qemu"
    if not (original / "initfs.cpio").exists():
        fail("build the test image first (make test-ota)")
    with TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        viewer = tmp / "input.py"
        viewer.write_text(SCRIPT)
        viewer.chmod(0o755)
        served = tmp / "served"
        served.mkdir()
        bundles(served, original)
        serve_file = tmp / "serve"
        server = https_server(served, serve_file)
        image = tmp / "image"
        image.mkdir()
        disk = image / "stopwatch.disk"
        ota_boot.provision(disk, ota_bundle.qemu_image(original))
        run.IMAGE = image
        # QEMU loads the files of the slot that was chosen, not the build's.
        run.machine = partial(run.machine, image=image)
        apps = home_grid.apps((original / "initfs.cpio").read_bytes())
        os.environ.update({
            "OPEN_ICONS": json.dumps({name: home_grid.icon(i, apps) for i, name in enumerate(apps)}),
            "OTA_TOOLS": str(ROOT / "tools"), "OTA_DISK": str(disk), "OTA_SERVE": str(serve_file),
            "OTA_SYSTEM": str(SYSTEM_VERSION),
            "VIOLET_NET_HOSTS": json.dumps({key: f"127.0.0.1:{server.server_address[1]}" for key in (HOST, f"{HOST}:443")}),
        })

        def boot(stage):
            """One power-on through the model bootloader; the slot that ran."""
            slot = ota_boot.boot(disk, image)
            if slot is None:
                print(f"ota: boot ({stage}): the chosen slot did not load")
                return None
            os.environ["OTA_STAGE"] = stage
            print(f"ota: boot ({stage}) from slot {'AB'[slot]}: {ota_boot.read_control(disk).describe()}")
            try:
                status = run.main(str(viewer))
            except SystemExit as e:
                # A slot that does not start may also make QEMU stop (-no-reboot).
                if stage != "attempt":
                    raise
                print(f"ota: attempt ended: {e}")
                status = 1
            if status != 0 and stage != "attempt":
                fail(f"{stage} boot from slot {'AB'[slot]} (viewer status {status})")
            return slot

        if boot("install") != 0:
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
        if boot("healthy") != 1:
            fail("the trial did not boot from B")
        if not ota_boot.read_control(disk).slot(1)["successful"]:
            fail("B was not marked successful")
        if boot("broken") != 1:
            fail("B did not stay the boot slot once proven")
        c = ota_boot.read_control(disk)
        if not (c.slot(0)["priority"] == 15 and not c.slot(0)["successful"]):
            fail(f"A is not staged as a trial: {c.describe()}")
        attempts = 0
        while True:
            peek = ota_boot.read_control(disk)
            if not peek.bootable(0):
                break
            attempts += 1
            if attempts > ota_boot.ACTIVE_TRIES:
                fail("A was tried more often than its tries allow")
            os.environ["OTA_ATTEMPT_SECONDS"] = "45"
            slot = boot("attempt")
            if slot not in (0, None):
                fail(f"attempt {attempts} booted slot {slot}")
            if ota_boot.read_control(disk).slot(0)["successful"]:
                fail("the broken slot A was marked successful")
        if attempts != ota_boot.ACTIVE_TRIES:
            fail(f"A was tried {attempts} times, not {ota_boot.ACTIVE_TRIES}")
        if boot("healthy") != 1:
            fail("the bootloader did not fall back to B")
        c = ota_boot.read_control(disk)
        if c.bootable(0) or not c.slot(1)["successful"]:
            fail(f"after the fallback: {c.describe()}")
        server.shutdown()
        print("OTA verified: older and forged bundles refused, a signed update staged in B, booted as a trial and "
              f"marked successful; a release that does not start spent A's {attempts} tries and the phone fell back to B")


if __name__ == "__main__":
    if sys.argv[1:] == ["--prepare"]:
        prepare()
    else:
        main()
