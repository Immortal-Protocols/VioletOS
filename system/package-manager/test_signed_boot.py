"""QEMU: the supervisor boots a staged catalog only if every package in it is signed by a pinned key.

Local suite (`make test-packages`, not in CI). Each case stages one catalog at
the supervisor's staged-catalog window and reads the source it chose from the
report page: 1 = the staged catalog, 0 = the factory catalog in the boot image.
A second group puts a catalog in a bank of the package disk, as an install
leaves it: 3 = the installed catalog.

It also checks the version floors where they are wired into the supervisor
(parse): a catalog whose floor record a package does not meet, and, when the
image was built with a component above version 1, a staged package below the
factory's version. `make test-packages-floor` builds such an image
(VIOLET_PACKAGE_VERSIONS=radio=2) and runs this file against it.
"""
import binascii
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time

repo = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(repo / "tools"))
sys.path.insert(0, str(repo / "image/packages"))
import os
# This suite boots the 32-bit (AArch32) image: build/sel4/qemu, make qemu32-image.
os.environ["VIOLET_ARCH"] = "aarch32"
from run import IMAGE, machine
from package_format import HEADER, TRAILER, unpack
import sign

RADIO = 7
scratch = tempfile.TemporaryDirectory()
out = Path(scratch.name)


def catalog(blobs, tail=b""):
    """VCT1 without package_format.pack's checks: these fixtures are meant to be wrong.
    `tail` follows the packages, where the supervisor's floor record goes."""
    body = bytearray()
    for _, blob in sorted(blobs.items()):
        body += blob
        body += bytes(-len(body) % 4)
    body += tail
    return struct.pack("<4s3I", b"VCT1", len(body), len(blobs), binascii.crc32(body)) + bytes(body)


def floors(entries):
    return b"VFL1" + struct.pack("<I", len(entries)) + b"".join(struct.pack("<2I", i, v) for i, v in entries)


def boot_source(name, data, installed=False):
    """The catalog source the supervisor reports. `installed` puts `data` in bank 1
    of the package disk (system/package-manager/catalog_volume.rs) instead of staging it."""
    path = out / (name + ".bin")
    path.write_bytes(data)
    if installed:
        disk = out / "user.disk"
        disk.write_bytes(bytes(16 * 1024**2))
        header = b"VOL1" + struct.pack("<3I", 1, len(data), binascii.crc32(data))
        header += struct.pack("<I", binascii.crc32(header))
        volume = bytearray(4 * 1024**2)
        volume[: len(header)] = header
        volume[512 : 512 + len(data)] = data
        Path(str(disk) + ".packages").write_bytes(volume)
        cmd = machine(disk=disk) + ["-qmp", "stdio"]
    else:
        cmd = machine(catalogs=(path, None)) + ["-qmp", "stdio"]
    p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
    try:
        def request(execute, arguments=None):
            p.stdin.write(json.dumps(dict(execute=execute, arguments=arguments or {})) + "\n")
            p.stdin.flush()
            while True:
                line = p.stdout.readline()
                if not line:
                    raise RuntimeError("QEMU exited")
                if not line.startswith("{"):
                    continue
                message = json.loads(line)
                if "return" in message:
                    return message["return"]
                if "error" in message:
                    raise RuntimeError(message)

        while '"QMP"' not in p.stdout.readline():
            if p.poll() is not None:
                raise RuntimeError("QEMU exited")
        request("qmp_capabilities")
        sample = out / "report.raw"
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            request("pmemsave", dict(val=0x49300000, size=16, filename=str(sample)))
            report = struct.unpack("<4I", sample.read_bytes())
            # The supervisor writes the source, then the generation.
            if report[2]:
                return report[3]
            time.sleep(0.1)
        raise AssertionError(f"{name}: the supervisor did not report a generation")
    finally:
        p.kill()
        p.wait()


factory = unpack((IMAGE / "catalog.bin").read_bytes())
target, pinned = sign.load_keys(IMAGE / "package-keys.json")
assert target == "qemu"
for identity, blob in factory.items():
    sign.verify(blob, pinned, target)
developer = sign.config(repo, "qemu")
assert developer.keys[developer.key_id] == pinned.get(developer.key_id), "the image was built with another developer key"
radio = factory[RADIO]
size = struct.unpack_from("<I", radio, 16)[0]
shipped = sign.fields(radio)[1]

patched = bytearray(radio)
patched[HEADER + 64] ^= 1
struct.pack_into("<I", patched, 20, binascii.crc32(patched[HEADER : HEADER + size]))

stranger = out / "stranger.pem"
subprocess.run(["openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", stranger], check=True)
foreign = sign.Signer("qemu", developer.key_id, {developer.key_id: sign.public_point(stranger)}, private=stranger)
other_board = sign.Signer("moto", developer.key_id, developer.keys, private=developer.private)
body = radio[: HEADER + size]

cases = [
    ("the factory catalog, staged unchanged", dict(factory), 1),
    ("Radio signed again at a higher version", {**factory, RADIO: developer.sign(body, shipped + 1)}, 1),
    ("Radio patched, checksum recomputed, old signature", {**factory, RADIO: bytes(patched)}, 0),
    ("the patched Radio signed again by the developer key", {**factory, RADIO: sign.resign(bytes(patched), developer)}, 1),
    ("Radio signed by a key the image does not pin", {**factory, RADIO: foreign.sign(body, shipped)}, 0),
    ("Radio signed for the handset", {**factory, RADIO: other_board.sign(body, shipped)}, 0),
    ("Radio with an empty signature trailer", {**factory, RADIO: body + bytes(TRAILER)}, 0),
    ("Radio in the unsigned VPK1 format", {**factory, RADIO: b"VPK1" + body[4:] + bytes(TRAILER)}, 0),
    ("Radio with a version raised under the old signature",
     {**factory, RADIO: radio[: HEADER + size + 8] + struct.pack("<I", shipped + 1) + radio[HEADER + size + 12 :]}, 0),
]
for name, blobs, expected in cases:
    source = boot_source("case", catalog(blobs))
    assert source == expected, f"{name}: source {source}, expected {expected}"
    print(f"{'booted ' if expected else 'refused'}: {name}")
print("The supervisor booted only catalogs whose packages a pinned key signed.")

# Version floors, as parse() applies them. A catalog the supervisor wrote may
# end with a floor record; no package in it may be below that record, and
# nothing else may follow the packages.
newer = {**factory, RADIO: developer.sign(body, shipped + 1)}
floor_cases = [
    ("a floor record its packages meet", catalog(newer, floors([(RADIO, shipped + 1), (12, 5)])), 1),
    ("a floor record above the Radio beside it", catalog(newer, floors([(RADIO, shipped + 2)])), 0),
    ("a floor record with a zero version", catalog(newer, floors([(12, 0)])), 0),
    ("a floor record with IDs out of order", catalog(newer, floors([(12, 5), (11, 5)])), 0),
    ("a floor record with an ID out of range", catalog(newer, floors([(32, 1)])), 0),
    ("a floor record cut short", catalog(newer, floors([(12, 5), (13, 5)])[:-4]), 0),
    ("bytes after the packages that are not a floor record", catalog(newer, bytes(16)), 0),
]
if shipped > 1:
    # Only an image built with Radio above version 1 can show the factory floor.
    floor_cases += [
        ("Radio signed at a version below the boot image's", catalog({**factory, RADIO: developer.sign(body, shipped - 1)}), 0),
        ("Radio signed at the boot image's version", catalog({**factory, RADIO: developer.sign(body, shipped)}), 1),
    ]
for name, data, expected in floor_cases:
    source = boot_source("floor", data)
    assert source == expected, f"{name}: source {source}, expected {expected}"
    print(f"{'booted ' if expected else 'refused'}: {name}")

# The same checks on a catalog bank of the package disk, where installs live.
installed_cases = [
    ("an installed catalog with its floor record", catalog(newer, floors([(RADIO, shipped + 1), (12, 5)])), 3),
    ("an installed catalog whose Radio is below its floor record", catalog(newer, floors([(RADIO, shipped + 2)])), 0),
    ("an installed catalog with a patched Radio", catalog({**factory, RADIO: bytes(patched)}), 0),
]
if shipped > 1:
    installed_cases.append(("an installed catalog with Radio below the boot image's version",
                            catalog({**factory, RADIO: developer.sign(body, shipped - 1)}), 0))
for name, data, expected in installed_cases:
    source = boot_source("installed", data, installed=True)
    assert source == expected, f"{name}: source {source}, expected {expected}"
    print(f"{'booted ' if expected else 'refused'}: {name}")
if shipped > 1:
    print("The supervisor refused catalogs below the boot image's version floor and below their own floor record.")
else:
    print("The supervisor refused catalogs below their own floor record. Every component ships at version 1,\n"
          "so the boot image's floor was not exercised: run make test-packages-floor for that.")
