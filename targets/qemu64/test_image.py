"""Host tests for the qemu64 image layout and console check (make test-unit): milliseconds, no build."""
from pathlib import Path
import struct
import sys
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parents[0] / "board"))
import image
import run

KVA, KPA = 0xffffffffc0000000, 0x40000000


def elf64(entry, segments, machine=183):
    """A minimal ELF64 with PT_LOAD segments [(vaddr, paddr, data, memsz)]."""
    header = bytearray(64 + 56 * len(segments))
    header[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<HHIQQQIHHHHHH", header, 16, 2, machine, 1, entry, 64, 0, 0, 64, 56, len(segments), 0, 0, 0)
    body = b""
    for i, (vaddr, paddr, data, memsz) in enumerate(segments):
        struct.pack_into("<IIQQQQQQ", header, 64 + 56 * i, 1, 5, len(header) + len(body), vaddr, paddr, len(data), memsz, 4096)
        body += data
    return bytes(header) + body


class Layout(unittest.TestCase):
    def test_kernel_segments_become_one_image_at_its_region(self):
        elf = elf64(KVA, [(KVA, KPA, b"boot", 0x1000), (KVA + 0x10000, KPA + 0x10000, b"text", 4), (KVA + 0x20000, KPA + 0x20000, b"", 0x3000)])
        data, entry, vbase = image.place_kernel(elf, KPA, 0x400000)
        self.assertEqual((entry, vbase, len(data)), (KVA, KVA, 0x23000))
        self.assertEqual((data[:4], data[0x10000:0x10004], data[0x20000:]), (b"boot", b"text", bytes(0x3000)))

    def test_a_kernel_outside_its_region_is_refused(self):
        for elf in [
            elf64(KVA, [(KVA, KPA, b"x", 0x500000)]),                      # zeroed data runs past the region
            elf64(KVA, [(KVA, KPA - 0x1000, b"x", 4)]),                    # below it
            elf64(KVA, [(KVA, KPA, b"x", 4), (KVA + 0x2000, KPA + 0x3000, b"y", 4)]),  # two different offsets
            elf64(KVA + 0x500000, [(KVA, KPA, b"x", 4)]),                  # entry outside the image
            elf64(KVA, [(KVA, KPA, b"x", 4)], machine=40),                 # an AArch32 kernel
            elf64(KVA, [(KVA, KPA, b"xxxx", 2)]),                          # more file than memory
        ]:
            with self.assertRaises(image.ImageError):
                image.place_kernel(elf, KPA, 0x400000)
        with self.assertRaises(image.ImageError):
            image.elf64_segments(b"\x7fELF\x01\x01" + bytes(100))

    def test_boot_tables_map_what_the_stub_and_the_kernel_need(self):
        base = 0x40801000
        tables, lower, upper = image.boot_tables(base, KVA, KPA, 0x400000)
        self.assertEqual((len(tables), lower, upper), (5 * 4096, base, base + 2 * 4096))
        walk = lambda root, va: image.translate(tables, base, root, va)
        # Lower half: the first 4 GiB at their own addresses (the stub, the interrupt controller).
        for va in (0x080b0080, 0x40800000, 0x40801ff8, 0x7fffffff, 0xffffffff):
            self.assertEqual(walk(lower, va), va)
        self.assertIsNone(walk(lower, 0x1_0000_0000))
        # Upper half: exactly the kernel's region, and nothing after it.
        for offset in (0, 0x10000, 0x1fffff, 0x200000, 0x3fffff):
            self.assertEqual(walk(upper, (KVA + offset) & (1 << 48) - 1), KPA + offset)
        self.assertIsNone(walk(upper, (KVA + 0x400000) & (1 << 48) - 1))
        self.assertIsNone(walk(upper, (KVA - 0x200000) & (1 << 48) - 1))
        # RAM is cached and executable for the kernel; the device gigabyte is not executable.
        entry = lambda page, index: struct.unpack_from("<Q", tables, page * 4096 + index * 8)[0]
        self.assertEqual(entry(1, 1) & 0xfff, 0x711)
        self.assertEqual(entry(1, 0), 0x601 | 3 << 53)
        self.assertEqual(entry(4, 0), KPA | 0x711)

    def test_boot_tables_refuse_unaligned_or_lower_half_kernels(self):
        for args in [(0x40801008, KVA, KPA, 0x400000), (0x40801000, KVA + 0x1000, KPA, 0x400000),
                     (0x40801000, 0x40000000, KPA, 0x400000), (0x40801000, KVA + (511 << 21), KPA, 0x400000)]:
            with self.assertRaises(image.ImageError):
                image.boot_tables(*args)

    def test_regions_are_assembled_in_place_and_may_not_overlap(self):
        blob = image.assemble([(0x40400000, b"root"), (0x40000000, b"kernel")], 0x40000000)
        self.assertEqual((blob[:6], blob[0x400000:], len(blob)), (b"kernel", b"root", 0x400004))
        with self.assertRaises(image.ImageError):
            image.assemble([(0x40000000, b"kernel"), (0x40000004, b"root")], 0x40000000)


class Console(unittest.TestCase):
    def test_expected_lines_must_all_appear_in_order(self):
        good = ["noise"] + [line + " tail" for line in run.EXPECTED]
        self.assertIsNone(run.missing(good))
        # One check failing, a line missing, and lines out of order are all reported.
        failed = [line.replace("x7 never selects a kernel call: ok", "x7 never selects a kernel call: FAILED") for line in good]
        self.assertEqual(run.missing(failed), "x7 never selects a kernel call: ok")
        self.assertEqual(run.missing(good[:-1]), "qemu64: done")
        swapped = good[:]
        swapped[2], swapped[3] = swapped[3], swapped[2]
        self.assertIsNotNone(run.missing(swapped))
        self.assertEqual(run.missing([]), run.EXPECTED[0])


if __name__ == "__main__":
    unittest.main()
