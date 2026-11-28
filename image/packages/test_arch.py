"""Host tests for the package build's architecture parameter (make test-unit): milliseconds."""
from pathlib import Path
import struct
import sys
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import violet_arch


def elf(wide, sections):
    """A minimal ELF with section headers [(name, type, flags, addr, data)]."""
    names = b"\0" + b"".join(n.encode() + b"\0" for n, *_ in sections) + b".shstrtab\0"
    header = 64 if wide else 52
    body, rows, at = b"", [], header
    offset = 1
    for name, kind, flags, addr, data in sections:
        rows.append((offset, kind, flags, addr, at, len(data)))
        offset += len(name) + 1
        body += data
        at += len(data)
    rows.append((offset, 3, 0, 0, at, len(names)))
    body += names
    shoff = header + len(body)
    out = bytearray(header)
    out[:6] = b"\x7fELF" + bytes([2 if wide else 1, 1])
    if wide:
        struct.pack_into("<Q", out, 40, shoff)
        struct.pack_into("<3H", out, 58, 64, len(rows), len(rows) - 1)
        table = b"".join(struct.pack("<IIQQQQIIQQ", *row, 0, 0, 1, 0) for row in rows)
    else:
        struct.pack_into("<I", out, 32, shoff)
        struct.pack_into("<3H", out, 46, 40, len(rows), len(rows) - 1)
        table = b"".join(struct.pack("<10I", *row, 0, 0, 1, 0) for row in rows)
    return bytes(out) + body + table


class Arch(unittest.TestCase):
    def test_default_is_the_32_bit_build_unchanged(self):
        self.assertEqual(violet_arch.selected({}), "aarch32")
        self.assertEqual(violet_arch.selected({"VIOLET_ARCH": ""}), "aarch32")
        self.assertEqual(violet_arch.rust_target({}), "armv7a-none-eabi")
        self.assertEqual(violet_arch.require_components({}), "aarch32")
        # Only the Rust target is wired into a build today (the package builder,
        # below). clang_flags() and sections() are not called by any build yet:
        # these lines check only that clang_flags({}) states the flags
        # platform/sel4/loader/build.py passes today, in its order, so wiring
        # it in later cannot change the 32-bit build.
        loader = (HERE.parents[1] / "platform/sel4/loader/build.py").read_text()
        self.assertIn('"' + '","'.join(violet_arch.clang_flags({})[:4]) + '"', loader)
        self.assertIn('"-mno-unaligned-access"', loader)
        # And the builder takes its Rust target from here, with no second literal.
        builder = (HERE / "build.py").read_text()
        self.assertIn('"--target=" + violet_arch.rust_target()', builder)
        self.assertNotIn("armv7a-none-eabi", builder)
        self.assertIn("violet_arch.require_components()", builder)

    def test_aarch64_is_known_but_components_stop_with_reasons(self):
        env = {"VIOLET_ARCH": "aarch64"}
        self.assertEqual(violet_arch.rust_target(env), "aarch64-unknown-none")
        self.assertIn("--target=aarch64-none-elf", violet_arch.clang_flags(env))
        self.assertNotIn("-marm", violet_arch.clang_flags(env))
        with self.assertRaises(SystemExit) as stop:
            violet_arch.require_components(env)
        message = str(stop.exception)
        self.assertIn("not ported", message)
        self.assertIn("supervisor.c", message)
        self.assertIn("make qemu64-image", message)
        for path in ("system/supervisor/supervisor.c", "runtime/transport.c", "services/power/process.rs", "runtime/open/sys.rs"):
            self.assertTrue((HERE.parents[1] / path).is_file(), path)

    def test_an_unknown_architecture_stops_the_build(self):
        for name in ("arm64", "riscv64", "AARCH64"):
            with self.assertRaises(SystemExit):
                violet_arch.selected({"VIOLET_ARCH": name})

    def test_sections_read_alike_from_both_elf_classes(self):
        layout = [(".text", 1, 6, 0x100000, b"code" * 4), (".bss", 8, 3, 0x101000, b"")]
        for wide in (False, True):
            rows, names = violet_arch.sections(elf(wide, layout))
            self.assertEqual(names, 2)
            self.assertEqual(rows[0][1:4], (1, 6, 0x100000))
            self.assertEqual(rows[0][5], 16)
            self.assertEqual(rows[1][1:4], (8, 3, 0x101000))
            data = elf(wide, layout)
            self.assertEqual(data[rows[0][4]:rows[0][4] + rows[0][5]], b"code" * 4)
        with self.assertRaises(ValueError):
            violet_arch.sections(b"\x7fELF\x03\x01" + bytes(64))
        with self.assertRaises(ValueError):
            violet_arch.sections(b"not an ELF file at all")


if __name__ == "__main__":
    unittest.main()
