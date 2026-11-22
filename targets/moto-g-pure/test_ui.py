"""Exercise the handset's packaged userspace in QEMU with the modeled touch controller."""
from pathlib import Path
from tempfile import TemporaryDirectory
import hashlib
import json
import subprocess
import sys

root = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(root / "image/packages"))
from package_format import pack, unpack

moto = root / "build/sel4/moto"
qemu = root / "build/sel4/qemu"
packages = unpack((moto / "catalog.bin").read_bytes())
assert set(packages) == {1, 2, 3, 4, 5, 6, 7, 8, 10, 14, 21}
reference = unpack((qemu / "catalog.bin").read_bytes())
for identity in (1, 2, 3, 5, 10):
    assert packages[identity] == reference[identity], f"package {identity} differs"
assert (moto / "packages/package_manager.vpk").read_bytes() == (qemu / "packages/package_manager.vpk").read_bytes()
for identity in (4, 6, 7, 8):
    assert packages[identity] != reference[identity]
    packages[identity] = reference[identity]
with TemporaryDirectory() as tmp:
    directory = Path(tmp)
    del packages[21]  # Moto USB hardware is absent on the QEMU target.
    catalog = pack(packages.values())
    digest = hashlib.sha256(catalog).hexdigest()
    (directory / (digest + ".bin")).write_bytes(catalog)
    (directory / "active.json").write_text(json.dumps({"current": digest, "previous": None}))
    subprocess.run([sys.executable, root / "targets/qemu/test_guest.py",
                    "--stack", "handset-ui", "--state-dir", directory], check=True, timeout=30)
