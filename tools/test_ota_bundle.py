"""Tests for tools/ota_bundle.py, tools/ota_boot.py and phone_image.v2_payload_length.

    python3 tools/test_ota_bundle.py                 # the unit tests (seconds)
    python3 tools/test_ota_bundle.py --fixtures DIR  # manifests for test_ota.rs

Keys are made with openssl in a temporary directory; nothing here reads a
build directory.
"""
import hashlib
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "image/packages"))
import ota_boot  # noqa: E402
import ota_bundle  # noqa: E402
import phone_image  # noqa: E402
import sign  # noqa: E402


def new_key(path):
    subprocess.run(["openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", path], check=True)
    return path


class Keys:
    def __init__(self, directory, target="qemu"):
        d = Path(directory)
        self.developer = new_key(d / "developer.pem")
        self.release = new_key(d / "release.pem")
        self.stranger = new_key(d / "stranger.pem")
        self.points = {1: sign.public_point(self.release), 2: sign.public_point(self.developer)}
        self.dev = sign.Signer(target, 2, self.points, private=self.developer)
        self.root = sign.Signer(target, 1, self.points, private=self.release, release=True)
        self.other = sign.Signer(target, 2, {2: sign.public_point(self.stranger)}, private=self.stranger)


def raw_manifest(signer, fields, digest, key_id=None):
    """A manifest signed without ota_bundle's checks, for fixtures that must be refused."""
    signed = b"VOT1" + struct.pack("<2I", signer.key_id if key_id is None else key_id, fields["version"]) + \
        fields.get("target", signer.target) + struct.pack("<4I", fields["layout"], fields["length"],
                                                         fields.get("r1", 0), fields.get("r2", 0)) + digest
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "m"
        path.write_bytes(signed)
        der = subprocess.run(["openssl", "dgst", "-sha256", "-sign", signer.private, path],
                             capture_output=True, check=True).stdout
    return (signed + sign.der_to_raw(der)).ljust(512, b"\0")


IMAGE = bytes(range(256)) * 20 + b"tail"  # 5124 bytes: 11 sectors, the last one partial


def fixtures(directory):
    out = Path(directory)
    out.mkdir(parents=True, exist_ok=True)
    keys = Keys(out)
    digest = hashlib.sha256(ota_bundle.pad(IMAGE)).digest()
    base = {"version": 3, "layout": 1, "length": len(IMAGE)}
    files = {
        "good.manifest": ota_bundle.manifest(keys.dev, 3, 1, IMAGE, check_version=False),
        "release.manifest": ota_bundle.manifest(keys.root, 7, 2, IMAGE, check_version=False),
        "version0.manifest": raw_manifest(keys.dev, {**base, "version": 0}, digest),
        "other-target.manifest": raw_manifest(keys.dev, {**base, "target": b"MOTO"}, digest),
        "unknown-key.manifest": raw_manifest(keys.other, base, digest, key_id=5),
        "wrong-key.manifest": raw_manifest(keys.other, base, digest),
        "reserved.manifest": raw_manifest(keys.dev, {**base, "r2": 1}, digest),
        "layout.manifest": raw_manifest(keys.dev, {**base, "layout": 3}, digest),
        "empty.manifest": raw_manifest(keys.dev, {**base, "length": 0}, digest),
        "huge.manifest": raw_manifest(keys.dev, {**base, "length": 0xffffffff}, digest),
    }
    for name, data in files.items():
        (out / name).write_bytes(data)
    (out / "image.bin").write_bytes(ota_bundle.pad(IMAGE))
    # The keys and target, in the form verify.rs's build includes.
    sign.write_keys(keys.dev, out)
    for private in ("developer.pem", "release.pem", "stranger.pem"):
        (out / private).unlink()


class Bundle(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.keys = Keys(cls.directory.name)

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def test_manifest_layout_and_signature(self):
        data = ota_bundle.bundle(self.keys.dev, 4, ota_bundle.LAYOUT_QEMU, IMAGE, check_version=False)
        self.assertEqual(len(data), 512 + 11 * 512)
        key_id, version, target, layout, length, digest = ota_bundle.fields(data[:512])
        self.assertEqual((key_id, version, target, layout, length), (2, 4, b"QEMU", 1, len(IMAGE)))
        self.assertEqual(digest, hashlib.sha256(IMAGE + bytes(-len(IMAGE) % 512)).digest())
        self.assertEqual(data[128:512], bytes(384))
        self.assertEqual(ota_bundle.verify(data, self.keys.points, "qemu"), 4)

    def test_every_signed_byte_and_the_image_are_covered(self):
        data = bytearray(ota_bundle.bundle(self.keys.dev, 4, ota_bundle.LAYOUT_QEMU, IMAGE, check_version=False))
        for at in list(range(0, 128)) + [512, 512 + len(IMAGE) - 1]:
            bad = bytearray(data)
            bad[at] ^= 0x01
            with self.assertRaises(ValueError, msg=f"byte {at}"):
                ota_bundle.verify(bytes(bad), self.keys.points, "qemu")
        with self.assertRaises(ValueError):
            ota_bundle.verify(bytes(data[:-512]), self.keys.points, "qemu")
        with self.assertRaises(ValueError):
            ota_bundle.verify(bytes(data), self.keys.points, "moto")
        with self.assertRaises(ValueError):
            ota_bundle.verify(bytes(data), {1: self.keys.points[1]}, "qemu")
        stranger = ota_bundle.bundle(self.keys.other, 4, 1, IMAGE, check_version=False)
        with self.assertRaises(ValueError):
            ota_bundle.verify(stranger, self.keys.points, "qemu")

    def test_a_package_signature_is_not_a_manifest(self):
        # A VPK2 record signed by the same key: its message starts "VPK2".
        body = struct.pack("<4s5I", b"VPK2", 1, 7, 64, 64, 0) + bytes(64)
        record = self.keys.dev.sign(body, 3)
        with self.assertRaises(ValueError):
            ota_bundle.verify(record.ljust(4096, b"\0"), self.keys.points, "qemu")

    def test_bad_arguments(self):
        for version in (0, 1 << 32):
            with self.assertRaises(ValueError):
                ota_bundle.manifest(self.keys.dev, version, 1, IMAGE, check_version=False)
        with self.assertRaises(ValueError):
            ota_bundle.manifest(self.keys.dev, 1, 9, IMAGE, check_version=False)
        with self.assertRaises(ValueError):
            ota_bundle.manifest(self.keys.dev, 1, 1, b"", check_version=False)

    def test_the_version_is_the_images_own(self):
        def note(v, inverse=None):
            return ota_bundle.VERSION_NOTE + struct.pack("<2I", v, v ^ 0xffffffff if inverse is None else inverse)
        # A supervisor built with VIOLET_SYSTEM_VERSION=2, and the magic alone
        # elsewhere (ota.rs keeps it as a constant too).
        image = IMAGE + note(2) + b"x" * 100 + ota_bundle.VERSION_NOTE + bytes(8) + note(5, inverse=7)
        self.assertEqual(ota_bundle.image_version(image), 2)
        data = ota_bundle.bundle(self.keys.dev, 2, ota_bundle.LAYOUT_QEMU, image)
        self.assertEqual(ota_bundle.verify(data, self.keys.points, "qemu"), 2)
        # Signed as 3 around a version-2 image: the phone's floor after the
        # install would be 2, so this is refused.
        for version in (1, 3):
            with self.assertRaises(ValueError, msg=f"version {version}"):
                ota_bundle.bundle(self.keys.dev, version, ota_bundle.LAYOUT_QEMU, image)
        with self.assertRaises(ValueError):
            ota_bundle.image_version(IMAGE)
        with self.assertRaises(ValueError):
            ota_bundle.image_version(image + note(3))
        with self.assertRaises(ValueError):
            ota_bundle.bundle(self.keys.dev, 4, ota_bundle.LAYOUT_QEMU, IMAGE)
        # The command line reads the version from the image and refuses another.
        with tempfile.TemporaryDirectory() as d:
            files = {name: (b"Z" * 64 + note(6) if name == "zImage" else b"F") for name in ota_bundle.QEMU_FILES}
            for name, data in files.items():
                (Path(d) / name).write_bytes(data)
            container = ota_bundle.qemu_image(d)
            self.assertEqual(ota_bundle.image_version(container), 6)
            with self.assertRaisesRegex(SystemExit, "not the image's own"):
                ota_bundle.main(["qemu", "--version", "7", "--image", d, "-o", str(Path(d) / "out.vota")])
            self.assertFalse((Path(d) / "out.vota").exists())

    def test_container_round_trip_and_damage(self):
        files = {"zImage": b"Z" * 1000, "entry.bin": b"E" * 76, "handoff.dtb": b"\xd0\x0d" * 9, "initfs.cpio": b"C" * 4097}
        image = ota_bundle.container(files)
        self.assertEqual(len(image) % 512, 0)
        self.assertEqual(ota_bundle.unpack(image), files)
        for bad in (image[:100], b"XSL1" + image[4:], image[:4] + struct.pack("<I", 0) + image[8:]):
            with self.assertRaises(ValueError):
                ota_bundle.unpack(bad)
        cut = bytearray(image)
        struct.pack_into("<I", cut, 8 + 16 + 4, len(image))  # zImage length past the end
        with self.assertRaises(ValueError):
            ota_bundle.unpack(bytes(cut))
        for name in ("", "a/b", ".hidden", "x" * 17):
            with self.assertRaises(ValueError):
                ota_bundle.container({name: b"1"})

    def test_android_payload_length(self):
        page = 2048
        header = bytearray(1660)
        header[:8] = b"ANDROID!"
        struct.pack_into("<II", header, 36, page, 2)
        struct.pack_into("<I", header, 8, 5000)     # kernel
        struct.pack_into("<I", header, 16, 3000)    # ramdisk
        struct.pack_into("<I", header, 1648, 100)   # dtb
        struct.pack_into("<I", header, 1644, 1660)
        image = bytes(header).ljust(page, b"\0") + bytes(3 * page + 2 * page + page) + b"\0" * (16 << 10)
        self.assertEqual(phone_image.v2_payload_length(image), page + 3 * page + 2 * page + page)
        with self.assertRaises(ValueError):
            phone_image.v2_payload_length(image[:5000])
        with self.assertRaises(ValueError):
            phone_image.v2_payload_length(b"NOTANDRO" + image[8:])


class Bootloader(unittest.TestCase):
    """The QEMU bootloader model against the same record slot.rs is tested on."""

    GOLDEN = bytes.fromhex("5f62000042434142013a00007700f700000000000000000000000000e4fcd57e")

    def test_codec_matches_slot_rs(self):
        c = ota_boot.Control.decode(self.GOLDEN)
        self.assertEqual(c.encode(), self.GOLDEN)
        self.assertEqual(ota_boot.Control.initial(1).encode(), self.GOLDEN)
        self.assertEqual(c.suffix(), 1)
        for at in (4, 8, 12, 28):
            bad = bytearray(self.GOLDEN)
            bad[at] ^= 1
            with self.assertRaises(ValueError):
                ota_boot.Control.decode(bytes(bad))

    def test_trial_fallback_and_success(self):
        c = ota_boot.Control.initial(0)
        c.set_unbootable(1)
        self.assertEqual(c.choose(), 0)
        c.set_active(1, 0)
        for _ in range(6):
            self.assertEqual(c.choose(), 1)
        self.assertEqual(c.choose(), 0)
        self.assertEqual(c.suffix(), 0)
        c.set_active(1, 0)
        self.assertEqual(c.choose(), 1)
        c.mark_successful(1)
        for _ in range(10):
            self.assertEqual(c.choose(), 1)

    def test_disk_provision_and_boot(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            disk = d / "disk"
            files = {name: name.encode() * 50 for name in ota_bundle.QEMU_FILES}
            disk.write_bytes(b"\x5a" * (1 << 20))  # existing data must stay
            ota_boot.provision(disk, ota_bundle.container(files), geometry=ota_boot.SMALL)
            self.assertEqual(disk.read_bytes()[:1 << 20], b"\x5a" * (1 << 20))
            out = d / "boot"
            slot = ota_boot.boot(disk, out, geometry=ota_boot.SMALL)
            self.assertEqual(slot, 0)
            for name, data in files.items():
                self.assertEqual((out / name).read_bytes(), data)
            control = ota_boot.read_control(disk, ota_boot.SMALL)
            self.assertEqual(control.suffix(), 0)
            self.assertFalse(control.bootable(1))
            # Stage a trial in B by hand (what Storage does) and boot it; a
            # garbled container counts as a failed attempt.
            g = ota_boot.SMALL
            with disk.open("r+b") as f:
                f.seek(g.first[1] * 512)
                f.write(b"garbage!" * 64)
            control.set_active(1, 0)
            ota_boot.write_control(disk, control, g)
            for _ in range(6):
                self.assertIsNone(ota_boot.boot(disk, out, geometry=g))
            self.assertEqual(ota_boot.boot(disk, out, geometry=g), 0)
            self.assertFalse(ota_boot.read_control(disk, g).bootable(1))


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--fixtures":
        fixtures(sys.argv[2])
    else:
        unittest.main()
