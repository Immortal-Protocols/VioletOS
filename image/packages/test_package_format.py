"""Host tests for the signed package format (make test-unit). Needs the openssl CLI.

    python3 image/packages/test_package_format.py             run the tests
    python3 image/packages/test_package_format.py --fixtures DIR
        write signed and broken packages plus package_keys.rs for the Rust
        verifier's tests (system/package-manager/test_verify.sh)
"""

import binascii
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO / "system/package-manager"))
import package_format
from package_format import HEADER, TRAILER, floors, inspect, pack, unpack, version
import sign


def new_key(path):
    subprocess.run(["openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", path], check=True)
    return path


def public_pem(private, path):
    subprocess.run(["openssl", "ec", "-in", private, "-pubout", "-out", path], check=True, capture_output=True)
    return path


def unsigned(identity, size=8192, fill=0x5A, abi=None, code=4096, arch="aarch32"):
    image = bytearray([fill]) * size
    if 11 <= identity <= 14:
        image[-20:] = b"APP1" + b"NOTEBOOK".ljust(16, b"\0")
    abi = package_format.package_abi(identity, arch) if abi is None else abi
    return struct.pack("<4s5I", b"VPK2", abi, identity, code, size, binascii.crc32(image)) + bytes(image)


def raw_record(signer, body, key_id, version_word, target):
    """A record signed without sign()'s own checks, for fixtures that must be wrong."""
    message = body + b"VSG1" + struct.pack("<2I", key_id, version_word) + target
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "message"
        path.write_bytes(message)
        der = subprocess.run(["openssl", "dgst", "-sha256", "-sign", signer.private, path],
                             capture_output=True, check=True).stdout
    return message + sign.der_to_raw(der)


class Keys:
    def __init__(self, directory):
        d = Path(directory)
        self.developer = new_key(d / "developer.pem")
        self.release = new_key(d / "release.pem")
        self.stranger = new_key(d / "stranger.pem")
        self.release_public = public_pem(self.release, d / "release-root.pub.pem")
        self.points = {1: sign.public_point(self.release_public), 2: sign.public_point(self.developer)}
        self.dev = sign.Signer("qemu", 2, self.points, private=self.developer)
        self.root = sign.Signer("qemu", 1, self.points, private=self.release, release=True)
        self.other = sign.Signer("qemu", 2, {2: sign.public_point(self.stranger)}, private=self.stranger)


def fixtures(directory):
    out = Path(directory)
    out.mkdir(parents=True, exist_ok=True)
    keys = Keys(out)
    body = unsigned(7, size=12288)
    files = {
        "good.vpk": keys.dev.sign(body, 3),
        "release.vpk": keys.root.sign(body, 7),
        "unsigned.vpk": body,
        "version0.vpk": raw_record(keys.dev, body, 2, 0, b"QEMU"),
        "other-target.vpk": raw_record(keys.dev, body, 2, 3, b"MOTO"),
        "unknown-key.vpk": raw_record(keys.other, body, 5, 3, b"QEMU"),
        "wrong-key.vpk": keys.other.sign(body, 3),
    }
    for name, data in files.items():
        (out / name).write_bytes(data)
    # A detached developer signature in openssl's DER, as moto-package sends it.
    (out / "detached.bin").write_bytes(files["good.vpk"])
    (out / "detached.der").write_bytes(subprocess.run(
        ["openssl", "dgst", "-sha256", "-sign", keys.developer, out / "detached.bin"],
        capture_output=True, check=True).stdout)
    sign.write_keys(keys.dev, out)
    for private in ("developer.pem", "release.pem", "stranger.pem"):
        (out / private).unlink()


class Format(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.keys = Keys(cls.directory.name)

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def check(self, blob, keys=None):
        return inspect(blob, self.keys.points if keys is None else keys, "qemu")

    def test_signature_encoding_round_trips(self):
        for r, s in [(1, 1), (0x80 << 248, 0x7F << 248), (sign.ORDER - 1, 0xFF), (0xFF << 120, 1 << 255)]:
            raw = r.to_bytes(32, "big") + s.to_bytes(32, "big")
            self.assertEqual(sign.der_to_raw(sign.raw_to_der(raw)), raw)
        for bad in [b"", b"\x30\x00", b"\x30\x06\x02\x01\x00\x02\x01\x01", b"\x30\x06\x02\x01\x01\x02\x01\x01\x00",
                    b"\x31\x06\x02\x01\x01\x02\x01\x01", b"\x30\x07\x02\x01\x01\x02\x01\x01"]:
            with self.assertRaises(ValueError):
                sign.der_to_raw(bad)

    def test_signed_package_round_trip(self):
        blob = self.keys.dev.sign(unsigned(7), 4)
        self.assertEqual(len(blob), HEADER + 8192 + TRAILER)
        self.assertEqual(self.check(blob), 7)
        self.assertEqual(version(blob), 4)
        self.assertEqual(sign.fields(blob), (2, 4, b"QEMU"))
        self.assertEqual(sign.verify(blob, self.keys.points, "qemu"), 4)
        self.assertEqual(self.check(self.keys.root.sign(unsigned(7), 9)), 7)
        # An app keeps its APP1 title at the end of the image, before the trailer.
        self.assertEqual(self.check(self.keys.dev.sign(unsigned(11), 1)), 11)

    def test_any_changed_byte_is_refused(self):
        blob = self.keys.dev.sign(unsigned(7), 4)
        for at in [5, 9, 13, 17, 21, HEADER, HEADER + 4000, HEADER + 8191, HEADER + 8192 + 1,
                   HEADER + 8192 + 4, HEADER + 8192 + 8, HEADER + 8192 + 12, len(blob) - 64, len(blob) - 1]:
            bad = bytearray(blob)
            bad[at] ^= 1
            with self.assertRaises(ValueError, msg=f"byte {at}"):
                self.check(bytes(bad))

    def test_relabelled_package_with_fresh_checksum_is_refused(self):
        blob = bytearray(self.keys.dev.sign(unsigned(7), 4))
        struct.pack_into("<I", blob, 8, 6)  # Radio's bytes, offered as Storage
        self.assertEqual(inspect(bytes(blob)), 6)  # shape and CRC alone still pass
        with self.assertRaisesRegex(ValueError, "signature is invalid"):
            self.check(bytes(blob))
        patched = bytearray(self.keys.dev.sign(unsigned(7), 4))
        patched[HEADER + 100] ^= 0xFF
        struct.pack_into("<I", patched, 20, binascii.crc32(patched[HEADER : HEADER + 8192]))
        self.assertEqual(inspect(bytes(patched)), 7)
        with self.assertRaisesRegex(ValueError, "signature is invalid"):
            self.check(bytes(patched))
        raised = bytearray(self.keys.dev.sign(unsigned(7), 4))
        struct.pack_into("<I", raised, HEADER + 8192 + 8, 5)
        with self.assertRaisesRegex(ValueError, "signature is invalid"):
            self.check(bytes(raised))
        # resign() is the supported way to edit a package (tests): CRC and signature are redone.
        again = sign.resign(bytes(patched), self.keys.dev)
        self.assertEqual(self.check(again), 7)
        self.assertEqual(version(again), 4)

    def test_untrusted_signers_and_targets(self):
        body = unsigned(7)
        with self.assertRaisesRegex(ValueError, "signature is invalid"):
            self.check(self.keys.other.sign(body, 4))
        with self.assertRaisesRegex(ValueError, "does not trust"):
            self.check(raw_record(self.keys.other, body, 5, 4, b"QEMU"))
        with self.assertRaisesRegex(ValueError, "another target"):
            self.check(sign.Signer("moto", 2, self.keys.points, private=self.keys.developer).sign(body, 4))
        # A release image pins the release root alone: developer-signed packages are refused.
        with self.assertRaisesRegex(ValueError, "does not trust"):
            self.check(self.keys.dev.sign(body, 4), {1: self.keys.points[1]})
        self.assertEqual(self.check(self.keys.root.sign(body, 4), {1: self.keys.points[1]}), 7)

    def test_unsigned_and_malformed_packages(self):
        body = unsigned(7)
        blob = self.keys.dev.sign(body, 4)
        old = b"VPK1" + blob[4:]
        for bad, message in [(old, "unsigned VPK1"), (old[: HEADER + 8192], "unsigned VPK1"), (body, "invalid segment"),
                             (blob[:-1], "invalid segment"), (blob + b"\0", "invalid segment"),
                             (body + bytes(TRAILER), "no signature trailer"), (blob[:50], "truncated"),
                             (raw_record(self.keys.dev, body, 2, 0, b"QEMU"), "no signature trailer")]:
            with self.assertRaisesRegex(ValueError, message):
                inspect(bad)
        with self.assertRaises(ValueError):
            self.keys.dev.sign(body, 0)
        with self.assertRaises(ValueError):
            self.keys.dev.sign(body[:-1], 1)
        with self.assertRaisesRegex(ValueError, "checksum"):
            inspect(self.keys.dev.sign(body[:HEADER] + b"\0" + body[HEADER + 1 :], 1))
        # Window limits: the trailer does not count against the image. Services
        # with their own stack (Radio 7, Touch 4, ...) and the Open Runtime get
        # 0x80000; Runtime 0x38000; the launcher and catalog apps 0x3E000.
        self.assertEqual(inspect(self.keys.dev.sign(unsigned(7, size=0x80000), 1)), 7)
        self.assertEqual(inspect(self.keys.dev.sign(unsigned(4, size=0x80000), 1)), 4)
        self.assertEqual(inspect(self.keys.dev.sign(unsigned(24, size=0x80000), 1)), 24)
        self.assertEqual(inspect(self.keys.dev.sign(unsigned(1, size=0x3E000), 1)), 1)
        for identity, size in [(7, 0x81000), (4, 0x81000), (1, 0x3F000), (3, 0x39000), (24, 0x81000)]:
            with self.assertRaises(ValueError):
                inspect(self.keys.dev.sign(unsigned(identity, size=size), 1))
        # The handset's Radio is ABI 3 (128 KiB of private RAM) or ABI 4 (192 KiB); nothing else is.
        for abi in (3, 4):
            self.assertEqual(inspect(self.keys.dev.sign(unsigned(7, abi=abi), 1)), 7)
        for identity, abi in [(7, 2), (7, 5), (6, 3), (6, 4), (24, 4)]:
            with self.assertRaisesRegex(ValueError, "unsupported package or ABI"):
                inspect(self.keys.dev.sign(unsigned(identity, abi=abi), 1))

    def test_test_fixtures_that_edit_a_package(self):
        # What targets/moto-g-pure/test_boot.py does to make a stalled or
        # replaced service: change the image, then sign.resign.
        good = self.keys.dev.sign(unsigned(6, size=8192), 2)
        end = HEADER + struct.unpack_from("<I", good, 16)[0]
        edited = bytearray(good)
        edited[HEADER:end] = b"\xfe\xff\xff\xea".ljust(end - HEADER, b"\0")
        fixed = sign.resign(bytes(edited), self.keys.dev)
        self.assertEqual((len(fixed), fixed[HEADER:HEADER + 4]), (len(good), b"\xfe\xff\xff\xea"))
        self.assertEqual(self.check(fixed, self.keys.points), 6)
        self.assertEqual(version(fixed), 2)
        self.assertEqual(sorted(unpack(pack([self.keys.dev.sign(unsigned(i), 1) for i in (1, 3, 4, 5)] + [fixed]))), [1, 3, 4, 5, 6])
        # The VPK1 way those fixtures used (checksum over everything after the
        # header, old signature left in place) is refused at each step.
        old = bytearray(edited)
        struct.pack_into("<I", old, 20, binascii.crc32(old[HEADER:]))
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            inspect(bytes(old))
        struct.pack_into("<I", old, 20, binascii.crc32(old[HEADER:end]))
        with self.assertRaisesRegex(ValueError, "signature is invalid"):
            self.check(bytes(old), self.keys.points)
        # Overwriting through the end of the record destroys the trailer.
        gone = bytearray(good)
        gone[HEADER:] = bytes(len(gone) - HEADER)
        with self.assertRaisesRegex(ValueError, "no signature trailer"):
            inspect(bytes(gone))

    def test_the_same_code_built_for_two_targets(self):
        # targets/moto-g-pure/test_ui.py compares handset and QEMU packages:
        # the records differ (target tag, key, signature), the code does not,
        # and each supervisor accepts only its own.
        body = unsigned(5)
        handset_key = sign.Signer("moto", 2, {2: sign.public_point(self.keys.stranger)}, private=self.keys.stranger)
        for_qemu, for_handset = self.keys.dev.sign(body, 1), handset_key.sign(body, 1)
        self.assertNotEqual(for_qemu, for_handset)
        self.assertEqual(for_qemu[:-TRAILER], for_handset[:-TRAILER])
        self.assertEqual(inspect(for_qemu, self.keys.points, "qemu"), 5)
        with self.assertRaisesRegex(ValueError, "another target"):
            inspect(for_handset, self.keys.points, "qemu")
        with self.assertRaisesRegex(ValueError, "signature is invalid"):
            inspect(for_qemu[:-TRAILER] + for_handset[-TRAILER:-68] + b"QEMU" + for_handset[-64:], self.keys.points, "qemu")

    def test_each_supervisor_takes_only_its_own_architecture(self):
        # The 32-bit words are what they always were; AArch64 adds 0x64 in the top byte.
        self.assertEqual([package_format.package_abi(i) for i in (1, 3, 7)], [1, 2, 1])
        self.assertEqual([package_format.package_abi(i, "aarch64") for i in (1, 3, 7)],
                         [0x64000001, 0x64000002, 0x64000001])
        self.assertEqual(package_format.radio_abi(4, "aarch64"), 0x64000004)
        wide = {i: self.keys.dev.sign(unsigned(i, arch="aarch64"), 1) for i in (1, 3, 4, 5, 7)}
        narrow = {i: self.keys.dev.sign(unsigned(i), 1) for i in (1, 3, 4, 5, 7)}
        self.assertEqual(package_format.package_arch(wide[3]), "aarch64")
        self.assertEqual(package_format.package_arch(narrow[3]), "aarch32")
        for identity in wide:
            self.assertEqual(inspect(wide[identity], self.keys.points, "qemu", arch="aarch64"), identity)
            self.assertEqual(inspect(narrow[identity], self.keys.points, "qemu", arch="aarch32"), identity)
            with self.assertRaisesRegex(ValueError, "built for aarch64, not aarch32"):
                inspect(wide[identity], arch="aarch32")
            with self.assertRaisesRegex(ValueError, "built for aarch32, not aarch64"):
                inspect(narrow[identity], arch="aarch64")
        # The architecture is in the signed bytes: relabelling a package breaks its signature.
        relabelled = bytearray(narrow[5])
        struct.pack_into("<I", relabelled, 4, package_format.package_abi(5, "aarch64"))
        with self.assertRaisesRegex(ValueError, "signature is invalid"):
            inspect(bytes(relabelled), self.keys.points, "qemu", arch="aarch64")
        # Radio's private-RAM ABIs exist for both architectures, and only as their own.
        self.assertEqual(inspect(self.keys.dev.sign(unsigned(7, abi=package_format.radio_abi(4, "aarch64")), 1), arch="aarch64"), 7)
        with self.assertRaises(ValueError):
            inspect(self.keys.dev.sign(unsigned(7, abi=4), 1), arch="aarch64")
        # An unknown top byte is refused, whichever architecture asks.
        for arch in ("aarch32", "aarch64"):
            with self.assertRaisesRegex(ValueError, "unknown architecture"):
                inspect(self.keys.dev.sign(unsigned(5, abi=0x20000001), 1), arch=arch)
        # A catalog is of one architecture: mixing them fails, each packs alone.
        self.assertEqual(sorted(unpack(pack(wide.values(), arch="aarch64"), arch="aarch64")), [1, 3, 4, 5, 7])
        with self.assertRaises(ValueError):
            unpack(pack(wide.values(), arch="aarch64"), arch="aarch32")
        with self.assertRaises(ValueError):
            pack([*list(narrow.values())[:4], wide[7]], arch="aarch32")
        # With no arch given, the build's own (VIOLET_ARCH) decides; unset, it is AArch32.
        from unittest import mock
        with mock.patch.dict("os.environ", {"VIOLET_ARCH": ""}):
            self.assertEqual(package_format.selected_arch(), "aarch32")
            self.assertEqual(inspect(narrow[5]), 5)
        with mock.patch.dict("os.environ", {"VIOLET_ARCH": "aarch64"}):
            self.assertEqual(inspect(wide[5]), 5)
            with self.assertRaises(ValueError):
                inspect(narrow[5])
        with self.assertRaises(ValueError):
            package_format.selected_arch("arm64")

    def test_catalog_round_trip(self):
        blobs = [self.keys.dev.sign(unsigned(i, size=4096 + 4 * i + 1), i) for i in (1, 3, 4, 5, 7)]
        catalog = pack(blobs)
        self.assertEqual(catalog[:4], b"VCT1")
        back = unpack(catalog)
        self.assertEqual(sorted(back), [1, 3, 4, 5, 7])
        self.assertEqual([back[i] for i in sorted(back)], blobs)
        self.assertEqual({i: version(b) for i, b in back.items()}, {1: 1, 3: 3, 4: 4, 5: 5, 7: 7})
        with self.assertRaisesRegex(ValueError, "duplicate"):
            pack(blobs + [blobs[0]])
        with self.assertRaisesRegex(ValueError, "required"):
            pack(blobs[1:])
        with self.assertRaises(ValueError):
            unpack(catalog[:-4])
        # A VPK1 catalog (no trailers) is not a catalog any more.
        legacy = b"".join(b"VPK1" + b[4:-TRAILER] + bytes(-(len(b) - TRAILER) % 4) for b in blobs)
        with self.assertRaises(ValueError):
            unpack(struct.pack("<4s3I", b"VCT1", len(legacy), len(blobs), binascii.crc32(legacy)) + legacy)
        # A catalog the supervisor wrote may end with a floor record.
        self.assertEqual(floors(catalog), {})
        kept = pack(blobs, {7: 7, 12: 5, 22: 2})
        self.assertEqual(kept[len(catalog):], b"VFL1" + struct.pack("<7I", 3, 7, 7, 12, 5, 22, 2))
        self.assertEqual((unpack(kept), floors(kept)), (back, {7: 7, 12: 5, 22: 2}))
        # A package below the catalog's own floor, and malformed records.
        with self.assertRaisesRegex(ValueError, "older than the catalog"):
            pack(blobs, {7: 8})

        def with_tail(tail):
            body = catalog[16:] + tail
            return struct.pack("<4s3I", b"VCT1", len(body), len(blobs), binascii.crc32(body)) + body

        self.assertEqual(floors(with_tail(b"VFL1" + struct.pack("<3I", 1, 12, 5))), {12: 5})
        for tail in (
            b"VFL1" + struct.pack("<3I", 1, 7, 8),            # Radio 7 is below floor 8
            b"VFL1" + struct.pack("<I", 0),                   # empty record
            b"VFL1" + struct.pack("<3I", 2, 12, 5),           # count beyond the bytes
            b"VFL1" + struct.pack("<3I", 1, 12, 5) + bytes(4),  # trailing bytes
            b"VFL1" + struct.pack("<3I", 1, 12, 0),           # version 0
            b"VFL1" + struct.pack("<3I", 1, 32, 1),           # ID out of range
            b"VFL1" + struct.pack("<5I", 2, 12, 5, 11, 5),    # IDs out of order
            b"VFL1" + struct.pack("<5I", 2, 12, 5, 12, 6),    # repeated ID
            b"VFL2" + struct.pack("<3I", 1, 12, 5),
            bytes(16),
        ):
            with self.assertRaises(ValueError, msg=tail):
                unpack(with_tail(tail))
        # One MiB still bounds the catalog, trailers included.
        big = [self.keys.dev.sign(unsigned(i, size=0x3E000), 1) for i in (1, 4, 5, 6, 7)]
        with self.assertRaisesRegex(ValueError, "exceeds boot window"):
            pack(big + [self.keys.dev.sign(unsigned(3, size=0x38000), 1)])
        # The handset's banks take 1.25 MiB (catalog_volume.rs, supervisor.c
        # CATALOG_LIMIT); QEMU's windows stay at one MiB, and so does unpack's default.
        self.assertEqual((package_format.catalog_limit("moto"), package_format.catalog_limit("qemu")),
                         (package_format.HANDSET_LIMIT, package_format.LIMIT))
        handset = pack(big + [self.keys.dev.sign(unsigned(3), 1)], limit=package_format.HANDSET_LIMIT)
        self.assertGreater(len(handset), package_format.LIMIT)
        self.assertEqual(sorted(unpack(handset, package_format.HANDSET_LIMIT)), [1, 3, 4, 5, 6, 7])
        with self.assertRaises(ValueError):
            unpack(handset)
        with self.assertRaisesRegex(ValueError, "exceeds boot window"):
            pack(big + [self.keys.dev.sign(unsigned(3, size=0x38000), 1)], limit=package_format.HANDSET_LIMIT)

    def test_build_key_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            d = Path(directory)
            base = {"VIOLET_DEVELOPER_KEY_DIR": str(d / "dev"), "VIOLET_RELEASE_PUBLIC_KEY": str(d / "absent.pem")}
            # Developer build: one generated key, ID 2, reused on the next build.
            dev = sign.config(REPO, "qemu", base)
            self.assertEqual((dev.key_id, sorted(dev.keys), dev.release), (2, [2], False))
            self.assertEqual((d / "dev/developer-signing.pem").stat().st_mode & 0o777, 0o600)
            self.assertEqual(sign.config(REPO, "qemu", base).keys, dev.keys)
            blob = dev.sign(unsigned(7), 1)
            self.assertEqual(sign.verify(blob, dev.keys, "qemu"), 1)
            # With a release root present, a developer image also accepts release packages.
            both = sign.config(REPO, "qemu", {**base, "VIOLET_RELEASE_PUBLIC_KEY": str(self.keys.release_public)})
            self.assertEqual((both.key_id, sorted(both.keys), both.release), (2, [1, 2], False))
            # Release: fails closed without the root key or without a way to sign.
            release = {**base, "VIOLET_RELEASE": "1"}
            with self.assertRaisesRegex(SystemExit, "release root public key"):
                sign.config(REPO, "qemu", release)
            release["VIOLET_RELEASE_PUBLIC_KEY"] = str(self.keys.release_public)
            with self.assertRaisesRegex(SystemExit, "VIOLET_SIGNING_KEY"):
                sign.config(REPO, "qemu", release)
            with self.assertRaisesRegex(SystemExit, "not found"):
                sign.config(REPO, "qemu", {**release, "VIOLET_SIGNING_KEY": str(d / "none.pem")})
            # The wrong private key cannot produce a release package.
            wrong = sign.config(REPO, "qemu", {**release, "VIOLET_SIGNING_KEY": str(self.keys.developer)})
            with self.assertRaisesRegex(SystemExit, "does not verify"):
                wrong.sign(unsigned(7), 1)
            # The right one pins the release root alone: no developer key in the image.
            real = sign.config(REPO, "qemu", {**release, "VIOLET_SIGNING_KEY": str(self.keys.release)})
            self.assertEqual((real.key_id, sorted(real.keys), real.release), (1, [1], True))
            signed = real.sign(unsigned(7), 2)
            self.assertEqual(sign.verify(signed, real.keys, "qemu"), 2)
            with self.assertRaisesRegex(ValueError, "does not trust"):
                sign.verify(blob, real.keys, "qemu")
            self.assertFalse((d / "dev2").exists())
            # An external signer (HSM) is a command: bytes in, DER signature out.
            command = f"openssl dgst -sha256 -sign {self.keys.release}"
            external = sign.config(REPO, "moto", {**release, "VIOLET_SIGN_COMMAND": command})
            self.assertEqual(sign.verify(external.sign(unsigned(7), 5), external.keys, "moto"), 5)
            # Release mode is the exact value 1. Any other value stops the build
            # instead of quietly making a developer-keyed image,
            for value in ("true", "yes", "TRUE", "on", "2", " 1", "1 "):
                with self.assertRaisesRegex(SystemExit, "VIOLET_RELEASE="):
                    sign.config(REPO, "moto", {**release, "VIOLET_RELEASE": value, "VIOLET_SIGNING_KEY": str(self.keys.release)})
            # and so does a signing key or command without release mode.
            for flavour in ({}, {"VIOLET_RELEASE": "0"}, {"VIOLET_RELEASE": ""}):
                for name in ("VIOLET_SIGNING_KEY", "VIOLET_SIGN_COMMAND"):
                    with self.assertRaisesRegex(SystemExit, "without VIOLET_RELEASE=1"):
                        sign.config(REPO, "moto", {**base, **flavour, name: str(self.keys.release)})
            self.assertFalse((d / "dev/moto").exists())
            self.assertEqual(sign.config(REPO, "qemu", {**base, "VIOLET_RELEASE": "0"}).keys, dev.keys)
            # The build says which it made.
            self.assertRegex(sign.summary(real), r"^Package signing: RELEASE image\. Trusts only: release root \(ID 1\)\.$")
            self.assertRegex(sign.summary(both), r"DEVELOPER image, not for release\. Trusts: release root \(ID 1\), developer \(ID 2\)")
            # Test builds may raise versions; a release build takes them from components.json only.
            self.assertEqual(sign.version_overrides(dev, {}), {})
            self.assertEqual(sign.version_overrides(dev, {"VIOLET_PACKAGE_VERSIONS": "radio=2,audio=3"}), {"radio": 2, "audio": 3})
            for bad in ("radio", "radio=", "radio=0", "radio=x", "=2", "radio=2,", "radio=-1"):
                with self.assertRaisesRegex(SystemExit, "expected name=version"):
                    sign.version_overrides(dev, {"VIOLET_PACKAGE_VERSIONS": bad})
            with self.assertRaisesRegex(SystemExit, "not VIOLET_RELEASE=1"):
                sign.version_overrides(real, {"VIOLET_PACKAGE_VERSIONS": "radio=2"})
            # The generated key table.
            rust = sign.write_keys(real, d).read_text()
            self.assertIn("pub const RELEASE: bool = true;", rust)
            self.assertIn("pub const TARGET: u32 = 0x554d4551;", rust)
            self.assertEqual(rust.count("(1, ["), 1)
            self.assertNotIn("(2, [", rust)
            self.assertEqual(sign.load_keys(d / "package-keys.json"), ("qemu", real.keys))
            self.assertEqual(json.loads((d / "package-keys.json").read_text())["release"], True)

    def test_staging_tool_verifies_and_refuses_older_packages(self):
        import manage

        with tempfile.TemporaryDirectory() as directory:
            d = Path(directory)
            factory = d / "catalog.bin"
            factory.write_bytes(pack([self.keys.dev.sign(unsigned(i), 2) for i in (1, 3, 4, 5, 7)]))
            state = d / "installed"

            def install(blob):
                (d / "inbox.vpk").write_bytes(blob)
                manage.stage(state, factory, "install", d / "inbox.vpk")

            radio = lambda v, fill=0x11: self.keys.dev.sign(unsigned(7, fill=fill), v)
            # No pinned keys beside the catalog: nothing can be verified, nothing is staged.
            with self.assertRaisesRegex(ValueError, "cannot verify"):
                install(radio(3))
            sign.write_keys(self.keys.dev, d)
            with self.assertRaisesRegex(ValueError, "signature is invalid"):
                install(self.keys.other.sign(unsigned(7), 3))
            with self.assertRaisesRegex(ValueError, "older than the installed"):
                install(radio(1))
            self.assertEqual(manage.read_state(state)["current"], None)
            install(radio(4))
            current = manage.launch_catalogs(state)[0]
            self.assertEqual(version(unpack(current.read_bytes())[7]), 4)
            # Below the installed version, and (after a rollback) below the shipped one.
            with self.assertRaisesRegex(ValueError, "older than the installed"):
                install(radio(3))
            install(radio(4, fill=0x22))
            install(self.keys.dev.sign(unsigned(6), 1))
            with self.assertRaisesRegex(ValueError, "older than the installed"):
                install(radio(1))
            unsigned_radio = unsigned(7)
            with self.assertRaises(ValueError):
                install(unsigned_radio)
            # Removing a component does not forget its version: Storage 3 is
            # installed, removed, and then Storage 2 (never shipped) is refused.
            install(self.keys.dev.sign(unsigned(6), 3))
            manage.stage(state, factory, "uninstall", "storage")
            current = manage.launch_catalogs(state)[0].read_bytes()
            self.assertNotIn(6, unpack(current))
            self.assertEqual(floors(current), {6: 3, 7: 4})
            with self.assertRaisesRegex(ValueError, "version that was removed"):
                install(self.keys.dev.sign(unsigned(6), 2))
            install(self.keys.dev.sign(unsigned(6), 3))
            self.assertEqual(floors(manage.launch_catalogs(state)[0].read_bytes()), {6: 3, 7: 4})
            with self.assertRaises(ValueError):
                install(b"VPK1" + radio(9)[4:])


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--fixtures":
        fixtures(sys.argv[2])
    else:
        unittest.main()
