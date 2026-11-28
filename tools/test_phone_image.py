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


def synthetic_v2(cmdline=b"bootopt=64S3,32S1,32S1 buildvariant=user"):
    """A small Android v2 boot image shaped like the XT2163DL's: kernel, ramdisk, a DTB."""
    page = 2048
    kernel, ramdisk, dtb = b"K" * 3000, b"R" * 1000, b"D" * 500
    header = bytearray(phone_image.V2_HEADER_SIZE)
    header[:8] = b"ANDROID!"
    struct.pack_into("<10I", header, 8, len(kernel), 0x40008000, len(ramdisk), 0x51b00000, 0, 0, 0x47880000, page, 2, 0)
    header[64:64 + len(cmdline)] = cmdline
    struct.pack_into("<I", header, 1644, phone_image.V2_HEADER_SIZE)
    struct.pack_into("<IQ", header, 1648, len(dtb), 0x47880000)
    body = b"".join(s + bytes(-len(s) % page) for s in (kernel, ramdisk, dtb))
    return phone_image.rebuild_v2(bytes(header).ljust(page, b"\0") + body, ramdisk, kernel=kernel)


class AArch64Boot(unittest.TestCase):
    """The boot image LK starts in AArch64 (targets/moto-g-pure/build64.py)."""
    IMAGE = b"\0" * 0x38 + b"ARM\x64" + bytes(range(256)) * 64

    def test_header_asks_lk_for_a_64_bit_kernel(self):
        import gzip
        stock = synthetic_v2()
        boot = phone_image.aarch64_v2(stock, b"ramdisk", self.IMAGE)
        self.assertEqual(struct.unpack_from("<I", boot, 12)[0], 0x40080000)
        line = boot[64:64 + 512].split(b"\0")[0]
        self.assertEqual(line, b"bootopt=64S3,32N2,64N2 buildvariant=user")
        page, kernel_size = struct.unpack_from("<I", boot, 36)[0], struct.unpack_from("<I", boot, 8)[0]
        self.assertEqual(gzip.decompress(boot[page:page + kernel_size]), self.IMAGE)
        # Everything else the stock header said stays: load addresses, page size, version.
        for offset in (20, 32, 36, 40):
            self.assertEqual(boot[offset:offset + 4], stock[offset:offset + 4], offset)
        self.assertEqual(phone_image.v2_payload_length(boot) % page, 0)

    def test_only_an_arm64_image_is_taken(self):
        with self.assertRaises(ValueError):
            phone_image.aarch64_v2(synthetic_v2(), b"ramdisk", b"\0" * 4096)

    def test_a_command_line_without_bootopt_gets_one(self):
        boot = phone_image.set_bootopt(synthetic_v2(b"buildvariant=user"), b"64S3,32N2,64N2")
        self.assertEqual(boot[64:64 + 512].split(b"\0")[0], b"bootopt=64S3,32N2,64N2 buildvariant=user")

    def test_storage_layout_is_the_32_bit_builds(self):
        image = phone_image.handset_storage(b"boot" * 1000, 0x140000)
        marker = 16 * 1024 ** 2
        self.assertTrue(image[marker:marker + 512].startswith(b"VioletOS XT2163DL boot_b storage v1\n"))
        history = marker + 512 + 1024 ** 2
        self.assertTrue(image[history:].startswith(b"VioletOS boot history v1\n"))
        self.assertTrue(image[history + 1536:].startswith(b"VioletOS catalog banks v1\n"))
        # The size services/usb-diagnostics/host/moto-flash and storage_volume.rs expect.
        self.assertEqual(len(image), 20450816)
        with self.assertRaises(ValueError):
            phone_image.handset_storage(bytes(16 * 1024 ** 2 + 1))

    @unittest.skipUnless((FACTORY / "boot.img").is_file(), "no XT2163DL factory boot.img")
    def test_factory_header_keeps_its_fields(self):
        stock = (FACTORY / "boot.img").read_bytes()
        boot = phone_image.aarch64_v2(stock, b"ramdisk", self.IMAGE)
        self.assertEqual(boot[64:64 + 512].split(b"\0")[0], b"bootopt=64S3,32N2,64N2 buildvariant=user")
        for offset in (20, 32, 36, 40, 44):
            self.assertEqual(boot[offset:offset + 4], stock[offset:offset + 4], offset)


if __name__ == "__main__":
    unittest.main()
