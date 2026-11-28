"""Tests for the Android Verified Boot metadata in tools/phone_image.py (seconds).

Keys are made with openssl in a temporary directory. When the XT2163DL
factory files are present (VIOLET_FACTORY_DIR, or the default download
folder), Motorola's own vbmeta images are read with the same code: their
signatures verify under the key they carry and their boot descriptor matches
the factory boot image. Without them those checks are skipped.
"""
import hashlib
import os
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import phone_image  # noqa: E402

FACTORY = Path(os.environ.get("VIOLET_FACTORY_DIR", ROOT / "build/xt2163dl/S3RHS32.20-42-13-2-3-2/stock"))
IMAGE = bytes(range(256)) * 37 + b"boot image"
SALT = bytes(range(32))


class Key:
    def __init__(self, directory, bits):
        self.pem = Path(directory) / f"rsa{bits}.pem"
        subprocess.run(["openssl", "genrsa", "-out", self.pem, str(bits)], check=True, capture_output=True)
        text = subprocess.run(["openssl", "rsa", "-in", self.pem, "-noout", "-modulus"], check=True, capture_output=True).stdout
        self.modulus = int(text.decode().strip().split("=")[1], 16)

    def sign(self, message):
        return subprocess.run(["openssl", "dgst", "-sha256", "-sign", self.pem], input=message, check=True,
                              capture_output=True).stdout


class Vbmeta(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.key = Key(cls.directory.name, 2048)
        (Path(cls.directory.name) / "other").mkdir()
        cls.other = Key(Path(cls.directory.name) / "other", 2048)

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def make(self, **changes):
        arguments = dict(image=IMAGE, modulus=self.key.modulus, sign=self.key.sign, rollback_index=7, salt=SALT)
        arguments.update(changes)
        return phone_image.avb_vbmeta(**arguments)

    def test_layout_and_what_it_says(self):
        v = self.make()
        self.assertEqual(v[:4], b"AVB0")
        self.assertEqual(len(v) % 64, 0)
        self.assertEqual(struct.unpack_from(">2I", v, 4), (1, 0))
        said = phone_image.avb_verify(v, {"boot": IMAGE})
        self.assertEqual(said["key_bits"], 2048)
        self.assertEqual(said["key_sha256"], hashlib.sha256(phone_image.avb_public_key(self.key.modulus)).hexdigest())
        self.assertEqual((said["rollback_index"], said["rollback_index_location"], said["flags"]), (7, 0, 0))
        self.assertEqual(said["descriptors"], [("hash", "boot", len(IMAGE))])
        # Bytes of the partition after the image (VioletOS's storage window) are not covered.
        phone_image.avb_verify(v, {"boot": IMAGE + b"storage that changes"})

    def test_every_byte_is_covered(self):
        v = self.make()
        header_and_aux = set(range(256)) | set(range(256 + struct.unpack_from(">Q", v, 12)[0], len(v)))
        for at in range(len(v)):
            bad = bytearray(v)
            bad[at] ^= 0x04
            try:
                phone_image.avb_verify(bytes(bad), {"boot": IMAGE})
            except ValueError:
                continue
            # Only padding inside the authentication block may change unnoticed.
            self.assertNotIn(at, header_and_aux, f"byte {at} of the signed part changed unnoticed")
            self.assertGreaterEqual(at, 256 + 32 + 256, f"hash or signature byte {at} changed unnoticed")

    def test_a_changed_image_or_another_key_is_refused(self):
        v = self.make()
        for image in (IMAGE[:-1], IMAGE[:100] + b"X" + IMAGE[101:], b""):
            with self.assertRaisesRegex(ValueError, "boot does not match"):
                phone_image.avb_verify(v, {"boot": image})
        with self.assertRaisesRegex(ValueError, "no hash descriptor for dtbo"):
            phone_image.avb_verify(v, {"dtbo": IMAGE})
        # Signed by one key but carrying another's: the builder itself refuses.
        other = self.other
        with self.assertRaisesRegex(ValueError, "signature is invalid"):
            self.make(sign=other.sign)
        # The same image under the other key verifies, with the other key's identity:
        # a signature alone proves nothing until the bootloader pins the key.
        theirs = phone_image.avb_verify(self.make(modulus=other.modulus, sign=other.sign), {"boot": IMAGE})
        self.assertNotEqual(theirs["key_sha256"], phone_image.avb_verify(v)["key_sha256"])

    def test_bad_arguments(self):
        for changes in (dict(image=b""), dict(rollback_index=-1), dict(rollback_index=1 << 64), dict(salt=b"short"),
                        dict(modulus=(1 << 1023) + 1)):
            with self.assertRaises(ValueError):
                self.make(**changes)
        with self.assertRaises(ValueError):
            phone_image.avb_verify(b"AVB0" + bytes(100))

    @unittest.skipUnless((FACTORY / "vbmeta.img").is_file() and (FACTORY / "boot.img").is_file(), "factory files not present")
    def test_the_factory_metadata_reads_the_same_way(self):
        boot = (FACTORY / "boot.img").read_bytes()
        said = phone_image.avb_verify((FACTORY / "vbmeta.img").read_bytes(), {"boot": boot})
        self.assertEqual(said["key_bits"], 2048)
        self.assertEqual(said["rollback_index_location"], 0)
        self.assertIn(("hash", "boot", 22202368), said["descriptors"])
        chains = [d for d in said["descriptors"] if d[0] == "chain"]
        self.assertEqual([(d[1], d[2]) for d in chains], [("vbmeta_system", 2)])
        system = phone_image.avb_verify((FACTORY / "vbmeta_system.img").read_bytes())
        # One Motorola key signs both and is the chained key.
        self.assertEqual(system["key_sha256"], said["key_sha256"])
        self.assertEqual(chains[0][3], said["key_sha256"])
        self.assertEqual(system["rollback_index"], said["rollback_index"])
        # A VioletOS boot image is not what Motorola signed.
        with self.assertRaisesRegex(ValueError, "boot does not match"):
            phone_image.avb_verify((FACTORY / "vbmeta.img").read_bytes(), {"boot": b"V" + boot[1:]})


if __name__ == "__main__":
    unittest.main()
