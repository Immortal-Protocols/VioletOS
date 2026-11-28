"""Tests for tools/boot_manifest.py and the rollback floor in tools/ota_boot.py.

    python3 tools/test_boot_manifest.py                 # the unit tests (seconds)
    python3 tools/test_boot_manifest.py --fixtures DIR  # manifests for boot/loader/test_verified.rs

Keys are made with openssl in a temporary directory; nothing here reads a
build directory, so it runs beside any other test.
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
import boot_manifest  # noqa: E402
import ota_boot  # noqa: E402
import ota_bundle  # noqa: E402
import sign  # noqa: E402
from test_ota_bundle import Keys  # noqa: E402

KERNEL = bytes(range(251)) * 9 + b"kernel"
ROOT_TASK = bytes(reversed(range(256))) * 13 + b"root task"
APPS = b"070701" + bytes(range(256)) * 5


def raw(signer, key_id=None, version=3, target=None, kernel_len=len(KERNEL), root_len=len(ROOT_TASK),
        apps_len=len(APPS), flags=1, apps_digest=None, magic=b"VBM1"):
    """A manifest signed without boot_manifest.build's checks, for fixtures that must be refused."""
    signed = magic + struct.pack("<2I", signer.key_id if key_id is None else key_id, version) + \
        (target or signer.target) + struct.pack("<4I", kernel_len, root_len, apps_len, flags) + \
        hashlib.sha256(KERNEL).digest() + hashlib.sha256(ROOT_TASK).digest() + \
        (hashlib.sha256(APPS).digest() if apps_digest is None else apps_digest)
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "m"
        path.write_bytes(signed)
        der = subprocess.run(["openssl", "dgst", "-sha256", "-sign", signer.private, path],
                             capture_output=True, check=True).stdout
    return signed + sign.der_to_raw(der)


def fixtures(directory):
    out = Path(directory)
    out.mkdir(parents=True, exist_ok=True)
    keys = Keys(out)
    update = ota_bundle.manifest(keys.dev, 3, 1, KERNEL, check_version=False)
    package = keys.dev.sign(b"VPK2" + bytes(12) + struct.pack("<2I", 168, 0) + bytes(168), 3)
    files = {
        "bound.manifest": boot_manifest.build(keys.dev, 3, KERNEL, ROOT_TASK, APPS),
        "unbound.manifest": boot_manifest.build(keys.dev, 3, KERNEL, ROOT_TASK),
        "release.manifest": boot_manifest.build(keys.root, 7, KERNEL, ROOT_TASK, APPS),
        "version0.manifest": raw(keys.dev, version=0),
        "other-target.manifest": raw(keys.dev, target=b"MOTO"),
        "unknown-key.manifest": raw(keys.other, key_id=5),
        "wrong-key.manifest": raw(keys.other),
        "unbound-length.manifest": raw(keys.dev, flags=0, apps_digest=bytes(32)),
        "unbound-digest.manifest": raw(keys.dev, flags=0, apps_len=0),
        "flags.manifest": raw(keys.dev, flags=3),
        "bound-empty.manifest": raw(keys.dev, apps_len=0),
        # Things the same key signed that are not boot manifests.
        "package-as.manifest": package[:boot_manifest.MANIFEST],
        "update-as.manifest": update[:128].ljust(boot_manifest.MANIFEST, b"\0"),
        "update-signature.manifest": boot_manifest.build(keys.dev, 3, KERNEL, ROOT_TASK, APPS)[:128] + update[64:128],
        "kernel.bin": KERNEL,
        "root.bin": ROOT_TASK,
        "apps.bin": APPS,
        "floor9.words": struct.pack("<3I", *boot_manifest.floor_words(9)),
    }
    assert len(package) >= boot_manifest.MANIFEST
    for name, data in files.items():
        (out / name).write_bytes(data)
    sign.write_keys(keys.dev, out)
    for private in ("developer.pem", "release.pem", "stranger.pem"):
        (out / private).unlink()


class Manifest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.keys = Keys(cls.directory.name)

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def check(self, blob, kernel=KERNEL, root=ROOT_TASK, apps=APPS, **options):
        return boot_manifest.verify(blob, self.keys.points, "qemu", kernel, root, apps, **options)

    def test_layout_and_signature(self):
        m = boot_manifest.build(self.keys.dev, 3, KERNEL, ROOT_TASK, APPS)
        self.assertEqual(len(m), 192)
        self.assertEqual(m[:16], b"VBM1" + struct.pack("<2I", 2, 3) + b"QEMU")
        self.assertEqual(struct.unpack_from("<4I", m, 16), (len(KERNEL), len(ROOT_TASK), len(APPS), 1))
        self.assertEqual(m[32:128], b"".join(hashlib.sha256(x).digest() for x in (KERNEL, ROOT_TASK, APPS)))
        self.assertTrue(sign.openssl_verify(self.keys.points[2], m[:128], m[128:]))
        parsed = self.check(m, floor=3, require_apps=True)
        self.assertEqual((parsed.version, parsed.key_id, parsed.apps_len), (3, 2, len(APPS)))
        unbound = boot_manifest.build(self.keys.dev, 3, KERNEL, ROOT_TASK)
        self.assertEqual(struct.unpack_from("<2I", unbound, 24), (0, 0))
        self.assertEqual(unbound[96:128], bytes(32))
        self.check(unbound, apps=b"anything")

    def test_every_refusal(self):
        good = boot_manifest.build(self.keys.dev, 3, KERNEL, ROOT_TASK, APPS)
        for at in range(len(good)):
            bad = bytearray(good)
            bad[at] ^= 1
            with self.assertRaises(ValueError, msg=f"byte {at}"):
                self.check(bytes(bad))
        cases = [
            (dict(kernel=KERNEL + b"\0"), "kernel"), (dict(kernel=b"x" + KERNEL[1:]), "kernel"),
            (dict(root=ROOT_TASK[:-1]), "root task"), (dict(apps=APPS[:-1] + b"!"), "/apps"),
            (dict(apps=None), "/apps"), (dict(floor=4), "rollback floor"),
        ]
        for change, text in cases:
            with self.assertRaisesRegex(ValueError, text):
                self.check(good, **change)
        with self.assertRaisesRegex(ValueError, "another target"):
            boot_manifest.verify(good, self.keys.points, "moto", KERNEL, ROOT_TASK, APPS)
        with self.assertRaisesRegex(ValueError, "does not trust"):
            boot_manifest.verify(good, {1: self.keys.points[1]}, "qemu", KERNEL, ROOT_TASK, APPS)
        with self.assertRaisesRegex(ValueError, "signature"):
            self.check(raw(self.keys.other))
        with self.assertRaisesRegex(ValueError, "does not bind"):
            self.check(boot_manifest.build(self.keys.dev, 3, KERNEL, ROOT_TASK), require_apps=True)

    def test_what_may_be_signed(self):
        for arguments in [(0, KERNEL, ROOT_TASK), (1 << 32, KERNEL, ROOT_TASK), (1, b"", ROOT_TASK),
                          (1, KERNEL, b""), (1, KERNEL, ROOT_TASK, b"")]:
            with self.assertRaises(ValueError):
                boot_manifest.build(self.keys.dev, *arguments)
        # A release manifest always binds the /apps image.
        with self.assertRaisesRegex(ValueError, "release"):
            boot_manifest.build(self.keys.root, 1, KERNEL, ROOT_TASK)
        self.assertEqual(boot_manifest.parse(boot_manifest.build(self.keys.root, 1, KERNEL, ROOT_TASK, APPS)).key_id, 1)

    def test_find_in_a_loader_image(self):
        m = boot_manifest.build(self.keys.dev, 6, KERNEL, ROOT_TASK)
        decoy = b"VBM1" + b"\xff" * 188  # not a manifest: its flags do not parse
        image = b"loader" + decoy + KERNEL + ROOT_TASK + m + bytes(3)
        self.assertEqual(boot_manifest.find(image).version, 6)
        self.assertIsNone(boot_manifest.find(b"loader" + decoy + KERNEL))
        self.assertIsNone(boot_manifest.find(b""))

    def test_floor_records(self):
        self.assertEqual(boot_manifest.read_floor_record(bytes(512)), 0)
        for floor in (0, 1, 7, 0xFFFFFFFF):
            record = boot_manifest.floor_record(floor)
            self.assertEqual(len(record), 16)
            self.assertEqual(boot_manifest.read_floor_record(record.ljust(512, b"\0")), floor)
            for at in range(16):
                bad = bytearray(record)
                bad[at] ^= 0x10
                with self.assertRaises(ValueError, msg=f"floor {floor} byte {at}"):
                    boot_manifest.read_floor_record(bytes(bad))
        with self.assertRaises(ValueError):
            boot_manifest.floor_record(-1)
        words = boot_manifest.floor_words(5)
        self.assertEqual(words, (0x4C464256, 5, 0xFFFFFFFA))
        devices = boot_manifest.floor_devices(5)
        self.assertEqual(devices[1], f"loader,addr={0x47870000},data={0x4C464256},data-len=4")
        self.assertEqual(devices[3], f"loader,addr={0x47870004},data=5,data-len=4")
        self.assertEqual(len(devices), 6)


def note(version):
    """The VIOLETSYSVER note a root task built with VIOLET_SYSTEM_VERSION=version carries."""
    return ota_bundle.VERSION_NOTE + struct.pack("<2I", version, version ^ 0xFFFFFFFF)


class Version(unittest.TestCase):
    """build.py signs the root task's own version, never a re-read of the environment."""

    def test_the_note_decides(self):
        root = b"supervisor" + note(3) + b"rest"
        self.assertEqual(boot_manifest.system_version(root), 3)
        self.assertEqual(boot_manifest.system_version(root, "3"), 3)
        # An incremental relink with VIOLET_SYSTEM_VERSION unset or different.
        with self.assertRaisesRegex(ValueError, "built as version 3"):
            boot_manifest.system_version(root, "1")
        for bad in ("0", "", "x", "1000000000", "-3", " 3"):
            with self.assertRaisesRegex(ValueError, "1 to 999999999", msg=bad):
                boot_manifest.system_version(root, bad)
        # A release states its version.
        with self.assertRaisesRegex(ValueError, "release"):
            boot_manifest.system_version(root, None, release=True)
        self.assertEqual(boot_manifest.system_version(root, "3", release=True), 3)
        with self.assertRaises(ValueError):
            boot_manifest.system_version(b"no note here")


class Loader(unittest.TestCase):
    """build.py refuses a QEMU loader image that does not run the check."""

    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.keys = Keys(cls.directory.name)

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def test_check_loader(self):
        m = boot_manifest.build(self.keys.dev, 2, KERNEL, ROOT_TASK)
        code = b"".join(boot_manifest.LOADER_MARKS)
        boot_manifest.check_loader(b"loader" + code + KERNEL + ROOT_TASK + m, m)
        with self.assertRaisesRegex(ValueError, "verified boot"):
            boot_manifest.check_loader(b"loader" + KERNEL + ROOT_TASK + m, m)  # VIOLET_VERIFIED_BOOT dropped
        with self.assertRaisesRegex(ValueError, "exactly once"):
            boot_manifest.check_loader(b"loader" + code + KERNEL + ROOT_TASK, m)
        with self.assertRaisesRegex(ValueError, "exactly once"):
            boot_manifest.check_loader(b"loader" + code + m + m, m)
        other = boot_manifest.build(self.keys.dev, 9, KERNEL, ROOT_TASK)
        with self.assertRaisesRegex(ValueError, "not the signed one"):
            boot_manifest.check_loader(b"loader" + code + m + other, m)


class Floor(unittest.TestCase):
    """The bootloader model: it verifies a slot before loading it, against its
    own keys and floor; the floor follows verified slots marked successful."""

    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.keys = Keys(cls.directory.name)

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def image(self, version, inner=None, signer=None):
        files = {name: b"F" for name in ota_bundle.QEMU_FILES}
        files["zImage"] = b"loader" + note(version) + KERNEL + ROOT_TASK + boot_manifest.build(
            signer or self.keys.dev, version if inner is None else inner, KERNEL, ROOT_TASK)
        return ota_bundle.container(files)

    def write_slot(self, disk, slot, image, g, signer=None, manifest=True):
        with disk.open("r+b") as f:
            f.seek(g.first[slot] * 512)
            f.write(ota_bundle.pad(image).ljust((g.sectors - 1) * 512, b"\0"))
            f.write(ota_bundle.manifest(signer or self.keys.dev, ota_bundle.image_version(image),
                                        ota_bundle.LAYOUT_QEMU, image) if manifest else bytes(512))

    def stage(self, disk, slot, image, g, signer=None, manifest=True, successful=False):
        """What Storage's slot writes can do: the slot's sectors and bootloader_control."""
        self.write_slot(disk, slot, image, g, signer, manifest)
        control = ota_boot.read_control(disk, g)
        control.set_active(slot, control.suffix())
        s = control.slot(slot)
        control.put(slot, s["priority"], ota_boot.ACTIVE_TRIES, successful, s["verity"])
        ota_boot.write_control(disk, control, g)

    def mark(self, disk, slot, g):
        control = ota_boot.read_control(disk, g)
        control.mark_successful(slot)
        ota_boot.write_control(disk, control, g)

    def at_floor_five(self, directory, fallback=False):
        """A disk where A is v3 (v5 with `fallback`, so B's refusal has a
        slot to fall back to), B is v5 marked successful, and the floor is 5."""
        g = ota_boot.SMALL
        disk, out = Path(directory) / "disk", Path(directory) / "out"
        ota_boot.provision(disk, self.image(3), geometry=g, signer=self.keys.dev)
        self.assertEqual(ota_boot.boot(disk, out, geometry=g), 0)
        self.stage(disk, 1, self.image(5), g)
        self.assertEqual(ota_boot.boot(disk, out, geometry=g), 1)
        self.mark(disk, 1, g)
        self.assertEqual(ota_boot.boot(disk, out, geometry=g), 1)
        self.assertEqual(ota_boot.read_floor(disk, g), 5)
        if fallback:
            self.write_slot(disk, 0, self.image(5), g)
        return g, disk, out

    def refused(self, disk, out, g, slot, text):
        """The next power-on refuses `slot` for `text` and boots the other."""
        reasons = []
        self.assertEqual(ota_boot.boot(disk, out, geometry=g, refusals=reasons), 1 - slot)
        self.assertEqual(len(reasons), 1)
        self.assertRegex(reasons[0], text)
        self.assertFalse(ota_boot.read_control(disk, g).bootable(slot))

    def test_floor_follows_success_and_persists(self):
        g = ota_boot.SMALL
        with tempfile.TemporaryDirectory() as directory:
            disk, out = Path(directory) / "disk", Path(directory) / "out"
            ota_boot.provision(disk, self.image(3), geometry=g, signer=self.keys.dev)
            self.assertEqual(ota_boot.read_floor(disk, g), 0)
            self.assertEqual(ota_boot.read_keys(disk, g), self.keys.points)
            # The factory slot is marked successful: its version is the floor from the first boot.
            self.assertEqual(ota_boot.boot(disk, out, geometry=g), 0)
            self.assertEqual(ota_boot.read_floor(disk, g), 3)
            self.assertEqual((out / "rollback-floor").read_text(), "3\n")
            # A newer system on trial does not raise it: a failed trial must fall back.
            self.stage(disk, 1, self.image(5), g)
            self.assertEqual(ota_boot.boot(disk, out, geometry=g), 1)
            self.assertEqual(ota_boot.read_floor(disk, g), 3)
            self.assertEqual((out / "rollback-floor").read_text(), "3\n")
            # Marked successful, it does, at the next power-on, and across later ones.
            self.mark(disk, 1, g)
            self.assertEqual(ota_boot.boot(disk, out, geometry=g), 1)
            self.assertEqual(ota_boot.read_floor(disk, g), 5)
            self.assertEqual(ota_boot.boot(disk, out, geometry=g), 1)
            self.assertEqual((out / "rollback-floor").read_text(), "5\n")

    def test_old_successful_slot_put_in_front_falls_back(self):
        # A rewritten control record puts the old (v3, still marked successful)
        # system first: the model refuses it below the floor, marks it
        # unbootable and boots B in the same power-on, and keeps doing so.
        with tempfile.TemporaryDirectory() as directory:
            g, disk, out = self.at_floor_five(directory)
            control = ota_boot.read_control(disk, g)
            control.set_active(0, 1)
            self.assertTrue(control.slot(0)["successful"])
            ota_boot.write_control(disk, control, g)
            self.refused(disk, out, g, 0, "below the rollback floor 5")
            self.assertEqual(ota_boot.boot(disk, out, geometry=g), 1)
            self.assertEqual((out / "rollback-floor").read_text(), "5\n")
            self.assertEqual(ota_boot.read_floor(disk, g), 5)

    def test_downgrades_written_into_a_slot(self):
        stranger = sign.Signer("qemu", 2, {2: self.keys.other.keys[2]}, private=self.keys.other.private)
        cases = [
            # An older validly signed image, written with its own manifest.
            (dict(image=self.image(3)), "below the rollback floor"),
            # A pre-verified-boot image: no update manifest in the slot.
            (dict(image=self.image(3), manifest=False), "not an OTA manifest"),
            # A developer build of someone else, whose loader pins its own key.
            (dict(image=self.image(7, signer=stranger), signer=stranger), "signature is invalid"),
        ]
        for change, text in cases:
            with self.subTest(text), tempfile.TemporaryDirectory() as directory:
                g, disk, out = self.at_floor_five(directory)
                self.stage(disk, 0, g=g, successful=True, **change)
                self.refused(disk, out, g, 0, text)
                self.assertEqual(ota_boot.read_floor(disk, g), 5)

    def test_a_forged_manifest_never_raises_the_floor(self):
        with tempfile.TemporaryDirectory() as directory:
            g, disk, out = self.at_floor_five(directory, fallback=True)
            # B's verified image with a well-formed but unsigned VBM1 block of
            # version 0xFFFFFFFF appended to its zImage, still marked successful.
            files = ota_bundle.unpack(self.image(5))
            forged = bytearray(boot_manifest.build(self.keys.dev, 5, KERNEL, ROOT_TASK))
            struct.pack_into("<I", forged, 8, 0xFFFFFFFF)
            files["zImage"] += bytes(forged)
            image = ota_bundle.container(files)
            with disk.open("r+b") as f:
                f.seek(g.first[1] * 512)
                f.write(ota_bundle.pad(image))
            reasons = []
            self.assertEqual(ota_boot.boot(disk, out, geometry=g, refusals=reasons), 0)
            self.assertRegex(reasons[0], "does not match its manifest")
            self.assertEqual(ota_boot.read_floor(disk, g), 5)
            # Signed by the trusted key, but its loader states another version than the slot.
            self.stage(disk, 1, self.image(6, inner=0xFFFFFFFF), g, successful=True)
            self.refused(disk, out, g, 1, "does not state the slot's version")
            self.assertEqual(ota_boot.read_floor(disk, g), 5)

    def test_every_slot_byte_is_checked(self):
        with tempfile.TemporaryDirectory() as directory:
            g, disk, out = self.at_floor_five(directory, fallback=True)
            image = ota_bundle.pad(self.image(5))
            for at in (0, 600, len(image) - 1, (g.sectors - 1) * 512 + 40, (g.sectors - 1) * 512 + 100):
                with disk.open("r+b") as f:
                    f.seek(g.first[1] * 512 + at)
                    byte = f.read(1)
                    f.seek(-1, 1)
                    f.write(bytes([byte[0] ^ 1]))
                self.refused(disk, out, g, 1, "slot B")
                with disk.open("r+b") as f:
                    f.seek(g.first[1] * 512 + at)
                    f.write(byte)
                self.stage(disk, 1, self.image(5), g, successful=True)
                self.assertEqual(ota_boot.boot(disk, out, geometry=g), 1)

    def test_both_slots_refused_boots_nothing(self):
        with tempfile.TemporaryDirectory() as directory:
            g, disk, out = self.at_floor_five(directory)
            self.stage(disk, 0, self.image(3), g, successful=True)
            with disk.open("r+b") as f:
                f.seek((g.first[1] + g.sectors - 1) * 512)
                f.write(bytes(512))
            reasons = []
            self.assertIsNone(ota_boot.boot(disk, out, geometry=g, refusals=reasons))
            self.assertEqual(len(reasons), 2)
            control = ota_boot.read_control(disk, g)
            self.assertFalse(control.bootable(0) or control.bootable(1))

    def test_a_damaged_floor_or_key_record_boots_nothing_and_spends_no_try(self):
        g = ota_boot.SMALL
        for sector, at in ((g.control + 1, 5), (g.control + 2, 20), (g.control + 2, 0)):
            with self.subTest(sector=sector, at=at), tempfile.TemporaryDirectory() as directory:
                disk, out = Path(directory) / "disk", Path(directory) / "out"
                ota_boot.provision(disk, self.image(3), geometry=g, signer=self.keys.dev)
                ota_boot.boot(disk, out, geometry=g)
                self.stage(disk, 1, self.image(4), g)
                before = ota_boot.read_control(disk, g).encode()
                with disk.open("r+b") as f:
                    f.seek(sector * 512 + at)
                    f.write(b"\x7f")
                self.assertIsNone(ota_boot.boot(disk, out, geometry=g))
                self.assertEqual(ota_boot.read_control(disk, g).encode(), before)

    def test_key_records(self):
        record = ota_boot.key_record(self.keys.points)
        self.assertEqual(ota_boot.read_key_record(record), self.keys.points)
        for bad in ({}, {2: b"\x04" + bytes(63)}, {0: self.keys.points[2]}, dict.fromkeys(range(1, 6), self.keys.points[2])):
            with self.assertRaises(ValueError):
                ota_boot.key_record(bad)
        with self.assertRaisesRegex(ValueError, "no trusted keys"):
            ota_boot.read_key_record(bytes(512))

    def test_provision_refuses_an_image_without_a_version(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "version note"):
                ota_boot.provision(Path(directory) / "disk", ota_bundle.container({n: b"F" for n in ota_bundle.QEMU_FILES}),
                                   geometry=ota_boot.SMALL, signer=self.keys.dev)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--fixtures":
        fixtures(sys.argv[2])
    else:
        unittest.main()
