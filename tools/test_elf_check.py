"""Host test for tools/elf_check.py (make test-unit; well under a second)."""
import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import elf_check


def elf(kind, segments, interp=None):
    """A minimal 32-bit ARM ELF: PT_LOAD (vaddr, memsz, flags) and maybe PT_INTERP naming `interp`."""
    headers = [(1, 0, vaddr, vaddr, 0, memsz, flags, 0x10000) for vaddr, memsz, flags in segments]
    path = b""
    if interp is not None:
        path = interp.encode() + b"\0"
        headers.append((3, 52 + 32 * (len(headers) + 1), 0, 0, len(path), len(path), 4, 1))
    ident = b"\x7fELF\x01\x01\x01" + bytes(9)
    head = ident + struct.pack("<HHIIIIIHHHHHH", kind, 40, 1, 0x10000, 52, 0, 0, 52, 32, len(headers), 40, 0, 0)
    return head + b"".join(struct.pack("<8I", *h) for h in headers) + path


def static_pie(relocations, textrel=False, relr=None, relro=(0x30000, 0x1000), sealed=True, phdr=True):
    """A static position-independent executable as lld lays one out: constants
    (with the relocation table) at 0, code at 0x10000, data at 0x30000 holding
    the dynamic section, all of it RELRO (`relro`: vaddr, size; None for no
    PT_GNU_RELRO), with relro_seal.o's mark unless `sealed` is false.
    `relocations` are (address, type); `relr` is a DT_RELR table."""
    table = b"".join(struct.pack("<II", address, kind) for address, kind in relocations)
    packed = b"".join(struct.pack("<I", w) for w in relr or [])
    tags = [(17, 0x1000), (18, len(table)), (19, 8)]
    if relr:
        tags += [(36, 0x1800), (35, len(packed)), (37, 4)]
    if textrel:
        tags.append((22, 0))
    dynamic = b"".join(struct.pack("<II", *t) for t in tags + [(0, 0)])
    headers = [(1, 0, 0, 0, 0x2000, 0x2000, 4, 0x10000), (1, 0x2000, 0x10000, 0x10000, 0, 0x1000, 5, 0x10000),
               (1, 0x2000, 0x30000, 0x30000, 0x1000, 0x1000, 6, 0x10000),
               (2, 0x2000, 0x30000, 0x30000, len(dynamic), len(dynamic), 6, 4)]
    if relro:
        headers.append((elf_check.PT_GNU_RELRO, 0x2000, relro[0], relro[0], relro[1], relro[1], 4, 1))
    if phdr:
        headers.insert(0, (elf_check.PT_PHDR, 52, 52, 52, 32 * (len(headers) + 1), 32 * (len(headers) + 1), 4, 4))
    ident = b"\x7fELF\x01\x01\x01" + bytes(9)
    head = ident + struct.pack("<HHIIIIIHHHHHH", 3, 40, 1, 0x10000, 52, 0, 0, 52, 32, len(headers), 40, 0, 0)
    image = bytearray(0x3000)
    blob = head + b"".join(struct.pack("<8I", *h) for h in headers)
    image[:len(blob)] = blob
    image[0x1000:0x1000 + len(table)] = table
    image[0x1800:0x1800 + len(packed)] = packed
    image[0x2000:0x2000 + len(dynamic)] = dynamic
    if sealed:
        image[0x1c00:0x1c00 + len(elf_check.SEAL_MARK)] = elf_check.SEAL_MARK
    return bytes(image)


SHIPPED = [(0x10000, 0xd8c, 4), (0x20d90, 0x8f60, 5), (0x39cf0, 0x310, 6), (0x49d30, 0x80c, 6)]


def static_pie64(relocations, relr=None, relro=(0x20000, 0x1000), sealed=True, interp=None, wx=False, code_at=0x10000, data_at=0x20000):
    """An AArch64 static PIE as ld.lld lays one out with its default 64 KiB
    max-page-size: constants at 0 (RELA table at 0x400, RELR at 0x600), code
    at `code_at`, data at `data_at` holding the dynamic section, its first
    page RELRO. Each in its own 64 KiB window, as the loader requires."""
    table = b"".join(struct.pack("<QQq", address, kind, 0) for address, kind in relocations)
    packed = b"".join(struct.pack("<Q", w) for w in relr or [])
    tags = [(7, 0x400), (8, len(table)), (9, 24)]
    if relr:
        tags += [(36, 0x600), (35, len(packed)), (37, 8)]
    dynamic = b"".join(struct.pack("<QQ", *t) for t in tags + [(0, 0)])
    # (type, flags, offset, vaddr, paddr, filesz, memsz, align)
    headers = [(1, 4, 0, 0, 0, 0x1000, 0x1000, 0x1000), (1, 5 | (2 if wx else 0), 0x1000, code_at, code_at, 0x1000, 0x1000, 0x1000),
               (1, 6, 0x2000, data_at, data_at, 0x1000, 0x2000, 0x1000),
               (2, 6, 0x2000, data_at, data_at, len(dynamic), len(dynamic), 8)]
    path = b""
    if interp is not None:
        path = interp.encode() + b"\0"
        headers.append((3, 4, 0x900, 0x900, 0x900, len(path), len(path), 1))
    if relro:
        headers.append((elf_check.PT_GNU_RELRO, 4, 0x2000, relro[0], relro[0], relro[1], relro[1], 1))
    headers.insert(0, (elf_check.PT_PHDR, 4, 64, 64, 64, 56 * (len(headers) + 1), 56 * (len(headers) + 1), 8))
    ident = b"\x7fELF\x02\x01\x01" + bytes(9)
    head = ident + struct.pack("<HHIQQQIHHHHHH", 3, 183, 1, code_at, 64, 0, 0, 64, 56, len(headers), 64, 0, 0)
    image = bytearray(0x3000)
    blob = head + b"".join(struct.pack("<IIQQQQQQ", *h) for h in headers)
    image[:len(blob)] = blob
    image[0x400:0x400 + len(table)] = table
    image[0x600:0x600 + len(packed)] = packed
    image[0x900:0x900 + len(path)] = path
    image[0x2000:0x2000 + len(dynamic)] = dynamic
    if sealed:
        image[0xc00:0xc00 + len(elf_check.SEAL_MARK)] = elf_check.SEAL_MARK
    return bytes(image)


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
        # Only musl's loader, and only when the image ships it.
        self.assertEqual(elf_check.problems(elf_check.inspect(elf(3, SHIPPED, interp=elf_check.LOADER))), [])
        self.assertEqual(elf_check.problems(elf_check.inspect(elf(3, SHIPPED, interp="/lib/ld-linux-armhf.so.3"))),
                         ["dynamically linked against /lib/ld-linux-armhf.so.3 (only /lib/ld-musl-armhf.so.1 is supported)"])
        dynamic = {"apps/d": elf(3, SHIPPED, interp=elf_check.LOADER)}
        with self.assertRaises(SystemExit) as refused:
            elf_check.check(dynamic)
        self.assertIn("the image has no /lib/ld-musl-armhf.so.1", str(refused.exception))
        dynamic["lib/ld-musl-armhf.so.1"] = elf(3, SHIPPED)
        self.assertEqual(elf_check.check(dynamic), [])

    def test_not_a_program(self):
        for data in [b"", b"#!/bin/sh\n", elf(2, SHIPPED)[:40], elf(1, SHIPPED), b"\x7fELF\x02" + bytes(60)]:
            self.assertIsNone(elf_check.inspect(data))

    def test_check_refuses_a_fixed_base_unless_it_is_a_named_test_program(self):
        files = {"apps/a": elf(2, SHIPPED), "apps/b": static_pie([]), "apps/notes.txt": b"hello"}
        # The release gate: a program that is not position-independent fails the build.
        with self.assertRaises(SystemExit) as refused:
            elf_check.check(files)
        self.assertIn("apps/a: not position-independent", str(refused.exception))
        self.assertNotIn("apps/b", str(refused.exception))
        # Only a name the build lists (a Go test program) gets through, and is reported.
        self.assertEqual(elf_check.check(files, fixed_ok={"apps/a"}), ["apps/a"])
        with self.assertRaises(SystemExit):
            elf_check.check(files, fixed_ok={"apps/other"})
        # A development build (VIOLET_OPEN_PIE=0) lists them all instead.
        self.assertEqual(elf_check.check(files, allow_fixed=True), ["apps/a"])
        # Being let through for a fixed base excuses nothing else.
        files["apps/bad"] = elf(2, [(0x10000, 64, 7)])
        for allowances in ({"fixed_ok": {"apps/a", "apps/bad"}}, {"allow_fixed": True}):
            with self.assertRaises(SystemExit) as refused:
                elf_check.check(files, **allowances)
            self.assertIn("apps/bad: a segment is writable and executable", str(refused.exception))
        self.assertEqual(elf_check.check({"apps/b": static_pie([])}), [])

    def test_a_static_pie_must_be_able_to_relocate_itself(self):
        RELATIVE, ABS32, GLOB_DAT = 23, 2, 21
        ok = static_pie([(0x30010, RELATIVE), (0x30ffc, RELATIVE), (0, 0)])
        info = elf_check.inspect(ok)
        self.assertTrue(info["pie"])
        self.assertEqual(info["relocations"], [(0x30010, RELATIVE), (0x30ffc, RELATIVE), (0, 0)])
        self.assertEqual(elf_check.problems(info), [])
        self.assertEqual(elf_check.check({"apps/ok": ok}), [])
        # A relative relocation in code or constants: the start code's write
        # would hit a read-only page and end the program before main.
        for address in (0x10040, 0x40, 0x30ffe, 0x31000, 0xfffffffc):
            why = elf_check.problems(elf_check.inspect(static_pie([(0x30010, RELATIVE), (address, RELATIVE)])))
            self.assertEqual(why, ["1 relocation(s) patch memory that is not writable (it would fault as it starts)"], hex(address))
        # Anything that needs a symbol: rcrt1 applies relative relocations only.
        why = elf_check.problems(elf_check.inspect(static_pie([(0x30010, ABS32), (0x30014, GLOB_DAT)])))
        self.assertEqual(why, ["relocations its start code cannot apply (types 2, 21; only relative ones)"])
        self.assertEqual(elf_check.problems(elf_check.inspect(static_pie([], textrel=True))),
                         ["it has text relocations (code is never writable)"])
        # Packed relative relocations (DT_RELR): an address, then a bitmap of the words after it.
        packed = elf_check.inspect(static_pie([], relr=[0x30010, 0b1011]))
        self.assertEqual(packed["relocations"], [(0x30010, RELATIVE), (0x30014, RELATIVE), (0x3001c, RELATIVE)])
        self.assertEqual(elf_check.problems(packed), [])
        self.assertEqual(elf_check.problems(elf_check.inspect(static_pie([], relr=[0x10010]))),
                         ["1 relocation(s) patch memory that is not writable (it would fault as it starts)"])
        # A table that is not in the file cannot be vouched for.
        broken = bytearray(ok)
        struct.pack_into("<I", broken, 0x2000 + 4, 0x7000_0000)   # DT_REL's address
        self.assertEqual(elf_check.problems(elf_check.inspect(bytes(broken))), ["its relocations cannot be read"])
        with self.assertRaises(SystemExit):
            elf_check.check({"apps/broken": bytes(broken)})
        # musl's dynamic loader binds its own symbols: the rule is not for it,
        # nor for a program it will relocate.
        symbolic = static_pie([(0x30010, GLOB_DAT)])
        self.assertEqual(elf_check.problems(elf_check.inspect(symbolic), loader=True), [])
        self.assertEqual(elf_check.check({"lib/ld-musl-armhf.so.1": symbolic}), [])
        with self.assertRaises(SystemExit):
            elf_check.check({"apps/symbolic": symbolic})


    def test_a_static_pie_seals_its_relro(self):
        # What lld makes with -z relro, plus tools/relro_seal.rs: loads.
        self.assertEqual(elf_check.problems(elf_check.inspect(static_pie([(0x30010, 23)]))), [])
        cases = [
            (dict(relro=None), "no PT_GNU_RELRO: its vtables and GOT would stay writable (link with -z relro)"),
            (dict(sealed=False), "it does not seal its RELRO read-only after relocating (link tools/relro_seal.rs; needs PT_PHDR)"),
            (dict(phdr=False), "it does not seal its RELRO read-only after relocating (link tools/relro_seal.rs; needs PT_PHDR)"),
            (dict(relro=(0x30000, 0x800)), "PT_GNU_RELRO does not end on a 4 KiB page: its last page would stay writable"),
            (dict(relro=(0x10000, 0x1000)), "PT_GNU_RELRO is not inside one writable segment"),
        ]
        for kwargs, why in cases:
            self.assertEqual(elf_check.problems(elf_check.inspect(static_pie([(0x30010, 23)], **kwargs))), [why], kwargs)
            with self.assertRaises(SystemExit):
                elf_check.check({"apps/x": static_pie([], **kwargs)})
        # A dynamically linked program is sealed by musl's loader, and the loader itself is exempt.
        self.assertEqual(elf_check.problems(elf_check.inspect(static_pie([], relro=None)), loader=True), [])

    def test_the_seal_source_carries_the_mark(self):
        source = (Path(__file__).resolve().parent / "relro_seal.rs").read_text()
        self.assertIn('*b"' + elf_check.SEAL_MARK[:-1].decode() + '\\0"', source)

    def test_only_a_development_build_keeps_fixed_programs(self):
        self.assertFalse(elf_check.fixed_allowed({}))
        self.assertFalse(elf_check.fixed_allowed({"VIOLET_OPEN_PIE": "1"}))
        self.assertTrue(elf_check.fixed_allowed({"VIOLET_OPEN_PIE": "0"}))
        self.assertTrue(elf_check.fixed_allowed({"VIOLET_OPEN_PIE": "0", "VIOLET_RELEASE": "0"}))
        with self.assertRaises(SystemExit):
            elf_check.fixed_allowed({"VIOLET_OPEN_PIE": "0", "VIOLET_RELEASE": "1"})
        root = Path(__file__).resolve().parents[1]
        for name in elf_check.FIXED_TEST_PROGRAMS:
            self.assertTrue((root / name / "main.go").exists(), name)
            self.assertTrue(elf_check.fixed_test_program(root, root / name, {}))
            self.assertFalse(elf_check.fixed_test_program(root, root / name, {"VIOLET_RELEASE": "1"}))
        # Any other Go folder (VIOLET_OPEN_EXTRA_APPS=/vendor/apps, say) is a shipped program.
        self.assertFalse(elf_check.fixed_test_program(root, root / "apps/examples/music_go", {}))
        self.assertFalse(elf_check.fixed_test_program(root, "/elsewhere/gohello", {}))

    def test_seal_object_and_a_real_static_pie(self):
        # Builds tools/relro_seal.rs and links a Rust program with the apps'
        # flags, when the host has the musl target (a few seconds); then the
        # gate passes it, and refuses the same link without the seal.
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import build_initfs
        import os
        import subprocess
        import tempfile
        try:
            rustc, lld = build_initfs.toolchain()
        except SystemExit:
            self.skipTest("no Rust musl target on this host")
        with tempfile.TemporaryDirectory(prefix="violet-relro-") as work:
            seal = build_initfs.relro_seal(Path(work) / "seal.o")
            self.assertIn(elf_check.SEAL_MARK, seal.read_bytes())
            self.assertIn(b".init_array", seal.read_bytes())
            program = Path(work) / "main.rs"
            program.write_text('trait T { fn f(&self) -> u32; }\nstruct A; impl T for A { fn f(&self) -> u32 { 7 } }\n'
                               'fn main() { let t: Box<dyn T> = Box::new(A); std::process::exit(t.f() as i32); }\n')
            if not build_initfs.pie():
                self.skipTest("VIOLET_OPEN_PIE=0")
            flags = list(map(str, build_initfs.pie_flags(rustc, seal)))
            out = Path(work) / "main"
            base = [rustc, "--edition=2021", "-O", "--target", build_initfs.TARGET, "-C", f"linker={lld}",
                    "-C", "linker-flavor=ld.lld", program, "-o", out]
            subprocess.run([*base, *flags], check=True, capture_output=True)
            info = elf_check.inspect(out.read_bytes())
            self.assertEqual(elf_check.problems(info), [])
            self.assertTrue(info["sealed"] and info["relro"] and info["phdr"])
            at = flags.index(f"link-arg={seal}")
            unsealed = flags[:at - 1] + flags[at + 1:]
            subprocess.run([*base, *unsealed], check=True, capture_output=True)
            self.assertIn("it does not seal its RELRO read-only after relocating (link tools/relro_seal.rs; needs PT_PHDR)",
                          elf_check.problems(elf_check.inspect(out.read_bytes())))

    def test_aarch64_programs_are_checked_by_their_own_rules(self):
        R_AARCH64_RELATIVE, R_AARCH64_ABS64 = 1027, 257
        good = static_pie64([(0x20000, R_AARCH64_RELATIVE), (0x20008, R_AARCH64_RELATIVE)], relr=[0x20010, 0b111])
        info = elf_check.inspect(good)
        self.assertEqual(info["arch"], "aarch64")
        self.assertTrue(info["pie"])
        # RELR: 0x20010, then the bitmap's two set bits after the marker: 0x20018 and 0x20020 (8-byte words).
        self.assertEqual(sorted(a for a, _ in info["relocations"]), [0x20000, 0x20008, 0x20010, 0x20018, 0x20020])
        self.assertEqual(elf_check.problems(info), [])
        self.assertEqual(elf_check.check({"apps/a": good}, arch="aarch64"), [])
        # The AArch32 gate does not read it (an AArch32 image never carried one), nor the AArch64 gate an AArch32 one.
        self.assertIsNone(elf_check.inspect(good, "aarch32"))
        self.assertIsNone(elf_check.inspect(elf(3, SHIPPED), "aarch64"))
        # The same refusals as on AArch32, by AArch64's numbers.
        why = elf_check.problems(elf_check.inspect(static_pie64([(0x20000, R_AARCH64_ABS64)])))
        self.assertIn("relocations its start code cannot apply (types 257; only relative ones)", why)
        why = elf_check.problems(elf_check.inspect(static_pie64([(0x10000, R_AARCH64_RELATIVE)])))
        self.assertIn("1 relocation(s) patch memory that is not writable (it would fault as it starts)", why)
        # An 8-byte patch that straddles the end of the writable segment is refused too.
        why = elf_check.problems(elf_check.inspect(static_pie64([(0x21ffc, R_AARCH64_RELATIVE)])))
        self.assertIn("1 relocation(s) patch memory that is not writable (it would fault as it starts)", why)
        self.assertIn("a segment is writable and executable", elf_check.problems(elf_check.inspect(static_pie64([], wx=True))))
        self.assertIn("it does not seal its RELRO read-only after relocating (link tools/relro_seal.rs; needs PT_PHDR)",
                      elf_check.problems(elf_check.inspect(static_pie64([], sealed=False))))
        self.assertIn("PT_GNU_RELRO does not end on a 4 KiB page: its last page would stay writable",
                      elf_check.problems(elf_check.inspect(static_pie64([], relro=(0x20000, 0x800)))))
        # Code sharing a 64 KiB window with data: the Open Runtime's W^X unit
        # (vma.rs WINDOW, procmgr.rs load's vma::mixed) on AArch64 too, though
        # its hardware pages are 4 KiB. Code and data on different 4 KiB pages
        # of one window (ld.lld -z max-page-size=4096) are refused, as the
        # loader refuses them.
        why = "code shares a 64 KiB page with data"
        self.assertIn(why, elf_check.problems(elf_check.inspect(static_pie64([], code_at=0x800))))
        self.assertIn(why, elf_check.problems(elf_check.inspect(static_pie64([], code_at=0x1000, data_at=0x3000, relro=(0x3000, 0x1000)))))
        self.assertIn(why, elf_check.problems(elf_check.inspect(static_pie64([], code_at=0x10000, data_at=0x13000, relro=(0x13000, 0x1000)))))
        self.assertNotIn(why, elf_check.problems(elf_check.inspect(static_pie64([], code_at=0x10000, data_at=0x20000))))
        # Only musl's AArch64 loader; the armhf one is refused.
        dynamic = static_pie64([], interp="/lib/ld-musl-aarch64.so.1")
        self.assertEqual(elf_check.problems(elf_check.inspect(dynamic)), [])
        armhf = static_pie64([], interp=elf_check.LOADER)
        self.assertIn("only /lib/ld-musl-aarch64.so.1 is supported", "; ".join(elf_check.problems(elf_check.inspect(armhf))))
        with self.assertRaises(SystemExit) as stop:
            elf_check.check({"apps/d": dynamic}, arch="aarch64")
        self.assertIn("the image has no /lib/ld-musl-aarch64.so.1", str(stop.exception))

    def test_the_image_gate_reads_every_elf_file(self):
        import build_initfs
        fixed = elf(2, SHIPPED)
        for name in ("apps/x", "bin/x", "usr/bin/helper", "lib/x", "sbin/x", "apps/folder/tool"):
            with self.assertRaises(SystemExit, msg=name) as refused:
                build_initfs.release_gate({name: (fixed, 0o100755), "etc/hosts": (b"x", 0o100644)}, set(), False)
            self.assertIn(f"{name}: not position-independent", str(refused.exception))
        self.assertEqual(build_initfs.release_gate({"usr/bin/helper": (static_pie([]), 0o100755)}, set(), False), [])
        self.assertEqual(build_initfs.release_gate({"usr/bin/helper": (fixed, 0o100755)}, set(), True), ["usr/bin/helper"])

    def test_the_python_package_is_gated(self):
        import build_python_package
        import os
        import tempfile
        if os.environ.get("VIOLET_OPEN_PIE") == "0":
            self.skipTest("VIOLET_OPEN_PIE=0")
        saved = build_python_package.PYTHON_ROOT
        try:
            with tempfile.TemporaryDirectory(prefix="violet-pypkg-") as work:
                build_python_package.PYTHON_ROOT = Path(work)
                (Path(work) / "bin").mkdir()
                for program, why in ((elf(2, SHIPPED), "not position-independent"),
                                     (static_pie([], sealed=False), "does not seal its RELRO")):
                    (Path(work) / "bin/python3").write_bytes(program)
                    with self.assertRaisesRegex(SystemExit, why):
                        build_python_package.archive()
        finally:
            build_python_package.PYTHON_ROOT = saved

if __name__ == "__main__":
    unittest.main()
