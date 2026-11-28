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


class Emulator(unittest.TestCase):
    """tools/run.py boots either architecture's image on the same machine and fixtures."""

    @classmethod
    def setUpClass(cls):
        import importlib.util
        spec = importlib.util.spec_from_file_location("violet_run", HERE.parents[1] / "tools/run.py")
        sys.path.insert(0, str(HERE.parents[1] / "tools"))
        cls.launcher = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.launcher)

    def test_aarch64_boots_its_plan_on_qemu_system_aarch64(self):
        import json
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            (folder / "qemu64.json").write_text(json.dumps({"board": "qemu64", "image": "qemu64.bin", "load": 0x48000000, "entry": 0x48000000}))
            wide = self.launcher.machine(folder, arch="aarch64")
            narrow = self.launcher.machine(folder, arch="aarch32")
        self.assertEqual(wide[:5], ["qemu-system-aarch64", "-machine", "virt,gic-version=3", "-cpu", "cortex-a53"])
        self.assertIn(f"loader,file={folder / 'qemu64.bin'},addr=0x48000000,force-raw=on", wide)
        self.assertIn("loader,addr=0x48000000,cpu-num=0", wide)
        self.assertFalse(any("zImage" in a or "entry.bin" in a for a in wide))
        self.assertEqual(narrow[:5], ["qemu-system-arm", "-machine", "virt,gic-version=3,virtualization=on", "-cpu", "max"])
        self.assertTrue(any(a.endswith("zImage,addr=0x45000000,force-raw=on") for a in narrow))
        # Everything after the boot loaders is the same machine: devices, fixtures, Wi-Fi records.
        boot_end = lambda cmd: max(i for i, a in enumerate(cmd) if "cpu-num=0" in a) + 1
        self.assertEqual(wide[5:wide.index("-device", 5)], narrow[5:narrow.index("-device", 5)])
        self.assertEqual(wide[boot_end(wide):], narrow[boot_end(narrow):])

    def test_an_image_of_the_other_architecture_is_refused(self):
        import json
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            (folder / "qemu64.json").write_text(json.dumps({"board": "qemu", "image": "x", "load": 0, "entry": 0}))
            with self.assertRaises(SystemExit):
                self.launcher.machine(folder, arch="aarch64")
        self.assertEqual(self.launcher.image_for("aarch64"), HERE.parents[1] / "build/sel4/qemu-aarch64")


class AppBuild(unittest.TestCase):
    """The open apps' build for each architecture (tools/build_initfs.py, tools/build_python.py)."""

    @classmethod
    def setUpClass(cls):
        sys.path.insert(0, str(HERE.parents[1] / "tools"))
        import build_initfs
        import build_python
        cls.initfs, cls.python = build_initfs, build_python

    def test_aarch32_keeps_the_original_choices(self):
        a = self.initfs.ARCHES["aarch32"]
        self.assertEqual(a["target"], "armv7-unknown-linux-musleabihf")
        self.assertEqual(a["cpu"], ["-C", "target-cpu=cortex-a53", "-C", "target-feature=+neon"])
        self.assertEqual(a["go"], {"GOARCH": "arm", "GOARM": "7"})
        self.assertEqual(a["loader"], self.initfs.LOADER)
        t = self.python.TARGETS["aarch32"]
        self.assertEqual((t.flags, t.triple, t.sysroot, t.pyroot), (self.python.FLAGS, "arm-linux-musleabihf",
                                                                     self.python.SYSROOT, self.python.PYROOT))
        self.assertEqual(t.wrapper, self.python.WORK / "bin/arm-linux-musleabihf-cc")
        self.assertEqual(a["c_flags"], self.python.FLAGS.split())

    def test_aarch64_is_armv8_0_in_its_own_directories(self):
        sys.path.insert(0, str(HERE.parents[0] / "board"))
        import board
        a, t = self.initfs.ARCHES["aarch64"], self.python.TARGETS["aarch64"]
        self.assertEqual(a["target"], board.ARCHES["aarch64"]["app_rust_target"])
        self.assertEqual(self.initfs.ARCHES["aarch32"]["target"], board.ARCHES["aarch32"]["app_rust_target"])
        self.assertIn("-mno-outline-atomics", t.flags)
        self.assertIn("-mno-outline-atomics", a["cflags"])
        self.assertIn("target-feature=-outline-atomics", a["cpu"])
        self.assertEqual(a["c_flags"], t.flags.split())
        self.assertEqual(a["go"], {"GOARCH": "arm64"})
        self.assertEqual(a["loader"], "lib/ld-musl-aarch64.so.1")
        self.assertTrue(str(t.sysroot).startswith(str(self.python.WORK / "aarch64")))
        self.assertNotEqual(a["seal"], self.initfs.ARCHES["aarch32"]["seal"])

    def test_the_release_gate_refuses_what_it_cannot_check(self):
        def program(elf_class, machine):
            head = bytearray(64)
            head[:6] = b"\x7fELF" + bytes([elf_class, 1])
            struct.pack_into("<HH", head, 16, 3, machine)
            return bytes(head)
        files = {"apps/a": program(2, 183), "apps/b": program(1, 40), "apps/c": program(2, 62)}
        out = self.initfs.refused_by_class(files, "aarch64", lambda data, arch=None: None)
        self.assertEqual([line.split(":")[0] for line in out], ["apps/a", "apps/b", "apps/c"])
        self.assertIn("does not read it", out[0])
        self.assertIn("not a little-endian aarch64", out[1])
        self.assertEqual(self.initfs.refused_by_class({"apps/a": files["apps/a"]}, "aarch64", lambda data, arch=None: {}), [])
        # The AArch32 gate is the one it always was: unread files are elf_check's to skip.
        with self.assertRaises(SystemExit):
            self.initfs.release_gate({"apps/a": (files["apps/a"], 0o100755)}, set(), False, "aarch64")

    def test_a_seal_object_must_have_an_init_array_entry(self):
        sys.path.insert(0, str(HERE.parents[1] / "image/packages"))
        from test_arch import elf
        with_entry = elf(True, [(".text", 1, 6, 0, b"code"), (".init_array", 14, 3, 0, bytes(8))])
        empty = elf(True, [(".text", 1, 6, 0, b"code"), (".init_array", 14, 3, 0, b"")])
        none = elf(False, [(".text", 1, 6, 0, b"code")])
        self.assertTrue(self.initfs.seals(with_entry))
        self.assertFalse(self.initfs.seals(empty))
        self.assertFalse(self.initfs.seals(none))



class Differences(unittest.TestCase):
    def test_the_supervisor_compiles_against_the_kernels_timer_setting(self):
        off, on = "KernelArmExportPTMRUser:BOOL=OFF\n", "x\nKernelArmExportPTMRUser:BOOL=ON\n"
        grant = off + "KernelEllisTimerGrant:INTERNAL=ON\n"
        disabled, enabled = "/* disabled: CONFIG_EXPORT_PTMR_USER */\n", "#define CONFIG_EXPORT_PTMR_USER  1\n"
        ellis = "#define CONFIG_PLAT_ELLIS  1\n"
        self.assertEqual(image.user_timer(off, [disabled, "#define CONFIG_OTHER 1\n"]), "")
        self.assertEqual(image.user_timer(on, [enabled]), "exported")
        self.assertEqual(image.user_timer(grant, [disabled + ellis + "#define CONFIG_ELLIS_TIMER_GRANT  1\n"]), "granted")
        # A stale tree's headers against this kernel: refused either way.
        with self.assertRaises(ValueError):
            image.user_timer(off, [enabled])
        with self.assertRaises(ValueError):
            image.user_timer(on, [disabled])
        with self.assertRaises(ValueError):
            image.user_timer("", [])
        # Headers from a kernel without the grant against one with it, and the reverse.
        with self.assertRaises(ValueError):
            image.user_timer(grant, [disabled + ellis])
        with self.assertRaises(ValueError):
            image.user_timer(off, [disabled + ellis + "#define CONFIG_ELLIS_TIMER_GRANT  1\n"])

    def test_the_lost_protections_are_named(self):
        self.assertEqual(image.differences("granted"), [image.NO_VERIFIED_BOOT])
        self.assertEqual(image.differences(""), [image.NO_VERIFIED_BOOT, image.NO_USER_TIMER])


if __name__ == "__main__":
    unittest.main()
