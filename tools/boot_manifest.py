"""The signed boot manifest the VioletOS loader checks (boot/loader/verified.rs).

    build(signer, version, kernel, root, apps=None) -> 192 bytes
    parse(blob) -> Manifest          fields only: no signature check
    verify(blob, keys, target, kernel, root, apps=None, floor=0) -> Manifest
    find(zimage) -> Manifest or None the manifest an image's loader carries

and the rollback floor record the QEMU bootloader model keeps and hands to
the loader (tools/ota_boot.py, tools/run.py):

    floor_record(floor) / read_floor_record(sector)   the stored record
    floor_devices(floor)                              QEMU arguments

`verify` mirrors the loader's check with openssl, for build-time checks and
the tests; the loader itself uses BearSSL.

    python3 tools/boot_manifest.py show build/sel4/qemu/zImage
"""
from collections import namedtuple
from pathlib import Path
import binascii
import hashlib
import struct
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "image/packages"))

MAGIC = b"VBM1"
SIGNED = 128
MANIFEST = SIGNED + 64
APPS_BOUND = 1
# Where tools/run.py puts the floor record for the QEMU loader (loader.c).
FLOOR_ADDRESS = 0x47870000
FLOOR_MAGIC = 0x4C464256  # "VBFL"
STORED_MAGIC = b"VRF1"

Manifest = namedtuple("Manifest", "key_id version target kernel_len root_len apps_len flags kernel root apps")


def build(signer, version, kernel, root, apps=None):
    """Sign a manifest for these images. `apps` (the /apps file system image)
    is bound only when given: a release must give it."""
    if not 0 < version < 1 << 32:
        raise ValueError("a system security version is 1 or more")
    if not kernel or not root or apps == b"":
        raise ValueError("an image to sign is empty")
    if signer.release and apps is None:
        raise ValueError("a release boot manifest must bind the /apps image")
    signed = MAGIC + struct.pack("<2I", signer.key_id, version) + signer.target + struct.pack(
        "<4I", len(kernel), len(root), len(apps) if apps is not None else 0, APPS_BOUND if apps is not None else 0)
    signed += hashlib.sha256(kernel).digest() + hashlib.sha256(root).digest()
    signed += hashlib.sha256(apps).digest() if apps is not None else bytes(32)
    assert len(signed) == SIGNED
    return signed + signer.signature(signed)


def parse(blob):
    """The fields of a manifest, unchecked beyond its shape."""
    blob = bytes(blob)
    if len(blob) != MANIFEST or blob[:4] != MAGIC:
        raise ValueError("not a boot manifest")
    key_id, version, target, kernel_len, root_len, apps_len, flags = struct.unpack_from("<2I4s4I", blob, 4)
    m = Manifest(key_id, version, target, kernel_len, root_len, apps_len, flags, blob[32:64], blob[64:96], blob[96:128])
    if flags & ~APPS_BOUND or (not flags and (apps_len or m.apps != bytes(32))):
        raise ValueError("boot manifest flags")
    return m


def verify(blob, keys, target, kernel, root, apps=None, floor=0, require_apps=False):
    """The loader's check, in its order; ValueError names the first failure."""
    from sign import TARGETS, openssl_verify
    m = parse(blob)
    if m.target != TARGETS[target]:
        raise ValueError("boot manifest was signed for another target")
    if m.key_id not in keys:
        raise ValueError("boot manifest is signed by a key this image does not trust")
    if not openssl_verify(keys[m.key_id], bytes(blob[:SIGNED]), bytes(blob[SIGNED:])):
        raise ValueError("boot manifest signature is invalid")
    if not m.version or m.version < floor:
        raise ValueError("boot manifest version is below the rollback floor")
    if require_apps and not m.flags & APPS_BOUND:
        raise ValueError("boot manifest does not bind the /apps image")
    if m.kernel_len != len(kernel) or hashlib.sha256(kernel).digest() != m.kernel:
        raise ValueError("kernel does not match the boot manifest")
    if m.root_len != len(root) or hashlib.sha256(root).digest() != m.root:
        raise ValueError("root task does not match the boot manifest")
    if m.flags & APPS_BOUND:
        if apps is None or m.apps_len != len(apps) or hashlib.sha256(apps).digest() != m.apps:
            raise ValueError("/apps image does not match the boot manifest")
    return m


def find(zimage):
    """The manifest inside a loader image, or None (an image built before
    verified boot, or a handset image, which carries none). The loader links
    it last, so the last well-formed candidate is taken."""
    zimage = bytes(zimage)
    at = zimage.rfind(MAGIC)
    while at >= 0:
        try:
            return parse(zimage[at:at + MANIFEST])
        except ValueError:
            at = zimage.rfind(MAGIC, 0, at)
    return None


def system_version(root_bin, requested=None, release=False):
    """The security version to sign: the VIOLETSYSVER note compiled into the
    root task (system/package-manager/ota.rs), which the running system and
    tools/ota_bundle.py go by. `requested` is VIOLET_SYSTEM_VERSION (None when
    unset): if given it must be that note's number, and a release must give it,
    so a release is never silently signed as version 1."""
    sys.path.insert(0, str(ROOT / "tools"))
    from ota_bundle import image_version
    version = image_version(bytes(root_bin))
    if requested is None:
        if release:
            raise ValueError("a release image needs VIOLET_SYSTEM_VERSION set explicitly")
        return version
    text = str(requested)
    if not text.isdigit() or not 0 < len(text) <= 9 or int(text) == 0:
        raise ValueError("VIOLET_SYSTEM_VERSION must be 1 to 999999999")
    if int(text) != version:
        raise ValueError(f"VIOLET_SYSTEM_VERSION={text}, but the root task was built as version {version}: rebuild it")
    return version


# Strings loader.c prints only from verified(): with --gc-sections they are
# in the image only when the check is compiled in and called (from prepare()).
LOADER_MARKS = (b"VioletOS loader: verified boot, system version ", b"VioletOS loader: boot refused, reason ")


def check_loader(zimage, manifest):
    """Refuse a QEMU loader image that does not run the check: it must carry
    `manifest` exactly once and the verified-boot code that reads it."""
    zimage = bytes(zimage)
    if zimage.count(bytes(manifest)) != 1:
        raise ValueError("the loader image does not carry its boot manifest exactly once")
    for mark in LOADER_MARKS:
        if mark not in zimage:
            raise ValueError("the loader image does not run verified boot (VIOLET_VERIFIED_BOOT missing?)")
    if find(zimage) != parse(manifest):
        raise ValueError("the loader image's last boot manifest is not the signed one")


def floor_record(floor):
    """The 16 bytes the bootloader model stores in misc."""
    if not 0 <= floor < 1 << 32:
        raise ValueError("rollback floor out of range")
    body = STORED_MAGIC + struct.pack("<2I", floor, floor ^ 0xFFFFFFFF)
    return body + struct.pack("<I", binascii.crc32(body))


def read_floor_record(sector):
    """The stored floor: 0 for a sector never written (all zeros), ValueError
    for a damaged record (the floor is then unknown: nothing may boot)."""
    raw = bytes(sector[:16])
    if raw == bytes(16):
        return 0
    if len(raw) < 16 or raw[:4] != STORED_MAGIC:
        raise ValueError("rollback floor record")
    floor, inverse, crc = struct.unpack_from("<3I", raw, 4)
    if inverse != floor ^ 0xFFFFFFFF or crc != binascii.crc32(raw[:12]):
        raise ValueError("rollback floor record")
    return floor


def floor_words(floor):
    """What the loader reads at FLOOR_ADDRESS (verified.rs handoff_floor)."""
    return (FLOOR_MAGIC, floor, floor ^ 0xFFFFFFFF)


def floor_devices(floor):
    """QEMU arguments that put the floor where the loader reads it."""
    cmd = []
    for i, value in enumerate(floor_words(floor)):
        cmd += ["-device", f"loader,addr={FLOOR_ADDRESS + 4 * i},data={value},data-len=4"]
    return cmd


def main(argv):
    if len(argv) == 2 and argv[0] == "show":
        m = find(Path(argv[1]).read_bytes())
        if m is None:
            sys.exit("no boot manifest in this image")
        print(f"system version {m.version}, key ID {m.key_id}, target {m.target.decode()}, "
              f"kernel {m.kernel_len} bytes, root task {m.root_len} bytes, "
              + (f"/apps image {m.apps_len} bytes" if m.flags & APPS_BOUND else "/apps image NOT bound"))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
