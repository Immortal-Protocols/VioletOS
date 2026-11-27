"""Host test for tools/elf_check.py (make test-unit; well under a second)."""
import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import elf_check


def elf(kind, segments, interp=False):
    """A minimal 32-bit ARM ELF: PT_LOAD (vaddr, memsz, flags) and maybe PT_INTERP."""
    headers = [(1, 0, vaddr, vaddr, 0, memsz, flags, 0x10000) for vaddr, memsz, flags in segments]
    if interp:
        headers.append((3, 0, 0, 0, 0, 0, 4, 1))
    ident = b"\x7fELF\x01\x01\x01" + bytes(9)
    head = ident + struct.pack("<HHIIIIIHHHHHH", kind, 40, 1, 0x10000, 52, 0, 0, 52, 32, len(headers), 40, 0, 0)
    return head + b"".join(struct.pack("<8I", *h) for h in headers)


SHIPPED = [(0x10000, 0xd8c, 4), (0x20d90, 0x8f60, 5), (0x39cf0, 0x310, 6), (0x49d30, 0x80c, 6)]


class ElfCheck(unittest.TestCase):
    def test_layout_like_the_shipped_apps_loads(self):
        info = elf_check.inspect(elf(2, SHIPPED))
        self.assertEqual(elf_check.problems(info), [])
        self.assertFalse(info["pie"])
        self.assertTrue(elf_check.inspect(elf(3, SHIPPED))["pie"])

    def test_wx_segment(self):
        self.assertEqual(elf_check.problems(elf_check.inspect(elf(2, [(0x10000, 64, 7)]))),
                         ["a segment is writable and executable"])

    def test_code_and_data_in_one_hardware_page(self):
        why = ["code shares a 64 KiB page with data"]
        self.assertEqual(elf_check.problems(elf_check.inspect(elf(2, [(0x10000, 64, 5), (0x11000, 64, 6)]))), why)
        # Constants in a page of code would be executable: refused like writable data.
        self.assertEqual(elf_check.problems(elf_check.inspect(elf(2, [(0x10000, 64, 5), (0x1f000, 64, 4)]))), why)
        self.assertEqual(elf_check.problems(elf_check.inspect(elf(2, [(0x10000, 0x800, 4), (0x10800, 64, 5)]))), why)
        self.assertEqual(elf_check.problems(elf_check.inspect(elf(2, [(0x10000, 64, 5), (0x10800, 64, 5)]))), [])
        self.assertEqual(elf_check.problems(elf_check.inspect(elf(2, [(0x10000, 64, 5), (0x20000, 64, 4)]))), [])
        # Rounded out to 4 KiB as the loader does: data ending at 0x1fff0 reaches 0x20000, not beyond.
        self.assertEqual(elf_check.problems(elf_check.inspect(elf(2, [(0x10000, 0xfff0, 6), (0x20000, 64, 5)]))), [])
        self.assertEqual(elf_check.problems(elf_check.inspect(elf(2, [(0x10000, 0x10001, 6), (0x21000, 64, 5)]))), why)

    def test_interpreter(self):
        self.assertEqual(elf_check.problems(elf_check.inspect(elf(3, SHIPPED, interp=True))), ["dynamically linked"])

    def test_not_a_program(self):
        for data in [b"", b"#!/bin/sh\n", elf(2, SHIPPED)[:40], elf(1, SHIPPED), b"\x7fELF\x02" + bytes(60)]:
            self.assertIsNone(elf_check.inspect(data))

    def test_check_names_what_fails_and_what_stays_fixed(self):
        files = {"apps/a": elf(2, SHIPPED), "apps/b": elf(3, SHIPPED), "apps/notes.txt": b"hello"}
        self.assertEqual(elf_check.check(files), ["apps/a"])
        files["apps/bad"] = elf(2, [(0x10000, 64, 7)])
        with self.assertRaises(SystemExit) as refused:
            elf_check.check(files)
        self.assertIn("apps/bad: a segment is writable and executable", str(refused.exception))


if __name__ == "__main__":
    unittest.main()
