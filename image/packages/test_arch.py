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
    def test_the_default_is_aarch64_and_the_32_bit_build_is_unchanged(self):
        self.assertEqual(violet_arch.selected({}), "aarch64")
        self.assertEqual(violet_arch.selected({"VIOLET_ARCH": ""}), "aarch64")
        self.assertEqual(violet_arch.rust_target({}), "aarch64-unknown-none")
        a32 = {"VIOLET_ARCH": "aarch32"}
        self.assertEqual(violet_arch.rust_target(a32), "armv7a-none-eabi")
        self.assertEqual(violet_arch.require_components(a32), "aarch32")
        # The package builder takes its Rust target and its ELF reader from
        # here (below). clang_flags() is not called by the AArch32 build:
        # these lines check only that clang_flags({}) states the flags
        # platform/sel4/loader/build.py passes today, in its order, so wiring
        # it in later cannot change the 32-bit build.
        loader = (HERE.parents[1] / "platform/sel4/loader/build.py").read_text()
        self.assertIn('"' + '","'.join(violet_arch.clang_flags(a32)[:4]) + '"', loader)
        self.assertIn('"-mno-unaligned-access"', loader)
        # And the builder takes its Rust target from here, with no second literal.
        builder = (HERE / "build.py").read_text()
        self.assertIn('"--target=" + violet_arch.rust_target()', builder)
        self.assertNotIn("armv7a-none-eabi", builder)
        self.assertIn("violet_arch.require_components()", builder)
        self.assertIn("violet_arch.sections(data)", builder)

    def test_aarch64_components_build_and_the_open_runtime_gap_is_named(self):
        env = {"VIOLET_ARCH": "aarch64"}
        self.assertEqual(violet_arch.rust_target(env), "aarch64-unknown-none")
        self.assertIn("--target=aarch64-none-elf", violet_arch.clang_flags(env))
        self.assertNotIn("-marm", violet_arch.clang_flags(env))
        self.assertEqual(violet_arch.UNPORTED, [])
        self.assertEqual(violet_arch.require_components(env), "aarch64")
        self.assertTrue(any("runtime/arch/enter64.rs" in line for line in violet_arch.APP_ABI_UNPORTED))
        # Anything left in UNPORTED stops the build again, naming it.
        saved = violet_arch.UNPORTED
        try:
            violet_arch.UNPORTED = ["system/supervisor/supervisor.c: example"]
            with self.assertRaises(SystemExit) as stop:
                violet_arch.require_components(env)
            self.assertIn("not ported", str(stop.exception))
            self.assertIn("supervisor.c", str(stop.exception))
        finally:
            violet_arch.UNPORTED = saved

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

    def test_the_builder_packages_both_elf_classes_alike(self):
        import build
        # The AArch32 linker script and flags are the ones the build always used.
        self.assertNotIn("eh_frame", build.component_script("aarch32"))
        self.assertIn("*(.ARM.exidx*) *(.ARM.extab*) }", build.component_script("aarch32"))
        self.assertEqual(build.link_flags("aarch32"), [])
        self.assertIn("*(.eh_frame*)", build.component_script("aarch64"))
        self.assertEqual(build.link_flags("aarch64"), ["-z", "max-page-size=4096"])
        code = bytes(range(256)) * 16
        layout = [(".text", 1, 6, 0x100000, code), (".data", 1, 3, 0x101000, b"data"),
                  (".bss", 8, 3, 0x101004, b"\xff" * 12), (".comment", 1, 0, 0, b"not loaded")]
        images = []
        for wide in (False, True):
            size, image = build.package_image(elf(wide, layout))
            self.assertEqual(size, 4096)
            self.assertEqual(bytes(image[:4096]), code)
            # .data, then .bss as zeroes (its file bytes are never copied).
            self.assertEqual(bytes(image[4096:]), b"data" + bytes(12))
            images.append(image)
        self.assertEqual(images[0], images[1])
        # The handset Radio's private RAM (NOBITS at 0x02800000) is left out of its image.
        radio = layout + [(".bss.private", 8, 3, 0x02800000, b"\xff" * 64)]
        self.assertEqual(len(build.package_image(elf(False, radio))[1]), 0x02800000 + 64 - 0x100000)
        for wide in (False, True):
            self.assertEqual(build.package_image(elf(wide, radio), 0x02800000)[1], images[0])
        for bad in ([(".text", 1, 6, 0x101000, code)], [(".text", 1, 6, 0x100000, code[:100])]):
            for wide in (False, True):
                with self.assertRaises(ValueError):
                    build.package_image(elf(wide, bad))


if __name__ == "__main__":
    unittest.main()
