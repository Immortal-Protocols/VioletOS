"""Build signed full-system update (OTA) bundles.

A bundle is one 512-byte manifest sector, then the slot image zero-padded to
whole sectors. The manifest format is documented in
system/package-manager/ota.rs, which the supervisor uses to check it; it is
signed with the same keys and target tags as VPK2 packages
(image/packages/sign.py), so a developer image accepts bundles signed by its
build directory's developer key and a release image only release-signed ones.

The slot image is what the bootloader starts:

- moto: the Android v2 boot image VioletOS's build writes (build/sel4/moto/
  boot.img) up to the end of its payload. The storage volume, boot history
  and catalog banks that follow it in boot_b are not part of a slot.
- qemu: a small container of the four files QEMU is started with (zImage,
  entry.bin, handoff.dtb, initfs.cpio), which tools/ota_boot.py (the QEMU
  model of a bootloader) unpacks:

      "VSL1" | count u32 | count x (name[16], offset u32, length u32) | data

  every file starting on a 512-byte boundary.

The manifest's version is the image's own: the supervisor carries the
VIOLET_SYSTEM_VERSION it was built with as a note (system/package-manager/
ota.rs, version_note), and once the phone runs the image that is its
rollback floor. A bundle whose --version differs from the note is refused,
so a release can never be labelled newer than the floor it installs.

    python3 tools/ota_bundle.py qemu -o build/ota/qemu.vota
    python3 tools/ota_bundle.py moto --version 2 -o build/ota/moto-v2.vota
"""
import argparse
import hashlib
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "image/packages"))
sys.path.insert(0, str(ROOT / "tools"))
import sign  # noqa: E402

SECTOR = 512
MANIFEST = 128
SIGNED = 64
MAGIC = b"VOT1"
LAYOUT_QEMU = 1
LAYOUT_ANDROID = 2
QEMU_FILES = ("zImage", "entry.bin", "handoff.dtb", "initfs.cpio")
CONTAINER = b"VSL1"
# ota.rs version_note: magic, version and its complement (u32 LE).
VERSION_NOTE = b"VIOLETSYSVER"


def pad(data):
    return bytes(data) + bytes(-len(data) % SECTOR)


def container(files):
    """The QEMU slot image: {name: bytes} in a VSL1 container."""
    names = list(files)
    if not names or len(names) > 20:
        raise ValueError("a slot container holds 1 to 20 files")
    header = bytearray(CONTAINER + struct.pack("<I", len(names)))
    offset = SECTOR
    table = []
    for name in names:
        raw = name.encode()
        if not raw or len(raw) > 16 or b"/" in raw or raw.startswith(b"."):
            raise ValueError(f"bad file name {name!r}")
        table.append((raw, offset, len(files[name])))
        offset += len(pad(files[name]))
    for raw, at, length in table:
        header += raw.ljust(16, b"\0") + struct.pack("<2I", at, length)
    if len(header) > SECTOR:
        raise ValueError("container header too large")
    body = bytearray(pad(header))
    for name in names:
        body += pad(files[name])
    return bytes(body)


def unpack(image):
    """{name: bytes} of a VSL1 container; ValueError if it is not one."""
    if len(image) < SECTOR or image[:4] != CONTAINER:
        raise ValueError("not a VioletOS QEMU slot container")
    count, = struct.unpack_from("<I", image, 4)
    if not 0 < count <= 20:
        raise ValueError("bad container file count")
    files = {}
    for i in range(count):
        raw, at, length = struct.unpack_from("<16s2I", image, 8 + 24 * i)
        name = raw.rstrip(b"\0").decode()
        if at % SECTOR or at < SECTOR or at + length > len(image) or not name or name in files:
            raise ValueError(f"bad container entry {name!r}")
        files[name] = bytes(image[at:at + length])
    return files


def qemu_image(directory):
    d = Path(directory)
    return container({name: (d / name).read_bytes() for name in QEMU_FILES})


def moto_image(boot):
    sys.path.insert(0, str(ROOT / "tools"))
    from phone_image import v2_payload_length
    data = Path(boot).read_bytes()
    return data[:v2_payload_length(data)]


def image_version(image):
    """The system version the image's supervisor was built with (its version
    notes, which must all agree); ValueError if it carries none."""
    found = set()
    at = image.find(VERSION_NOTE)
    while at >= 0:
        if at + 20 <= len(image):
            v, inverse = struct.unpack_from("<2I", image, at + 12)
            if v and inverse == v ^ 0xffffffff:
                found.add(v)
        at = image.find(VERSION_NOTE, at + 1)
    if not found:
        raise ValueError("the image carries no system version note (is it a VioletOS build?)")
    if len(found) > 1:
        raise ValueError(f"the image carries several system versions: {sorted(found)}")
    return found.pop()


def manifest(signer, version, layout, image, check_version=True):
    """The signed 512-byte manifest sector for `image`. Unless `check_version`
    is false (tests' deliberately wrong bundles), `version` must be the one
    the image was built with."""
    if not 0 < version < 1 << 32:
        raise ValueError("an OTA version is 1 or more")
    if check_version and image_version(bytes(image)) != version:
        raise ValueError(f"version {version} is not the image's own (built with VIOLET_SYSTEM_VERSION="
                         f"{image_version(bytes(image))})")
    if layout not in (LAYOUT_QEMU, LAYOUT_ANDROID) or not image or len(image) >= 1 << 32:
        raise ValueError("bad layout or image")
    digest = hashlib.sha256(pad(image)).digest()
    signed = MAGIC + struct.pack("<2I", signer.key_id, version) + signer.target + struct.pack(
        "<4I", layout, len(image), 0, 0) + digest
    assert len(signed) == SIGNED
    return (signed + signer.signature(signed)).ljust(SECTOR, b"\0")


def bundle(signer, version, layout, image, check_version=True):
    return manifest(signer, version, layout, image, check_version) + pad(image)


def fields(sector):
    """(key_id, version, target, layout, image_len, digest) of a manifest sector."""
    if len(sector) < MANIFEST or sector[:4] != MAGIC:
        raise ValueError("not an OTA manifest")
    key_id, version, target, layout, length, r1, r2 = struct.unpack_from("<2I4s4I", sector, 4)
    if r1 or r2:
        raise ValueError("reserved manifest fields are not zero")
    return key_id, version, target, layout, length, bytes(sector[32:64])


def verify(data, keys, target):
    """Check a bundle as the phone will (signature, target, digest); returns its version."""
    key_id, version, tag, layout, length, digest = fields(data[:SECTOR])
    if tag != sign.TARGETS[target]:
        raise ValueError("bundle was signed for another target")
    if key_id not in keys:
        raise ValueError("bundle is signed by a key this image does not trust")
    if not sign.openssl_verify(keys[key_id], bytes(data[:SIGNED]), bytes(data[SIGNED:MANIFEST])):
        raise ValueError("bundle signature is invalid")
    if not version:
        raise ValueError("bundle version 0")
    image = data[SECTOR:SECTOR + len(pad(bytes(length)))]
    if len(image) != len(pad(bytes(length))) or hashlib.sha256(image).digest() != digest:
        raise ValueError("bundle image does not match its manifest")
    return version


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("target", choices=("qemu", "moto"))
    p.add_argument("--version", type=int, help="security version of this system; must be the VIOLET_SYSTEM_VERSION "
                   "the image was built with (the default: read from the image)")
    p.add_argument("--image", help="QEMU image directory (default build/sel4/qemu) or Moto boot.img (default build/sel4/moto/boot.img)")
    p.add_argument("-o", "--output", required=True)
    a = p.parse_args(argv)
    if a.target == "qemu":
        image = qemu_image(a.image or ROOT / "build/sel4/qemu")
        layout = LAYOUT_QEMU
    else:
        image = moto_image(a.image or ROOT / "build/sel4/moto/boot.img")
        layout = LAYOUT_ANDROID
    try:
        version = image_version(image)
        if a.version is not None and a.version != version:
            raise ValueError(f"--version {a.version} is not the image's own: it was built with VIOLET_SYSTEM_VERSION={version}")
    except ValueError as e:
        raise SystemExit(f"ota_bundle: {e}")
    a.version = version
    signer = sign.config(ROOT, a.target)
    out = Path(a.output)
    out.parent.mkdir(parents=True, exist_ok=True)
    data = bundle(signer, version, layout, image)
    verify(data, signer.keys, a.target)
    out.write_bytes(data)
    print(f"{out}: {a.target} system version {a.version}, {len(image)} image bytes, key {signer.key_id}"
          f" ({'release' if signer.release else 'developer'}), sha256 {hashlib.sha256(data).hexdigest()}")


if __name__ == "__main__":
    main()
