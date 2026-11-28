"""Host tests for the board descriptions (make test-unit): milliseconds, nothing is built."""
import copy
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import board as boards


class Descriptions(unittest.TestCase):
    def test_every_board_is_valid_and_matches_the_code(self):
        self.assertEqual(boards.names(), ["qemu", "qemu64", "xt2163dl"])
        for name in boards.names():
            self.assertEqual(boards.drift(boards.load(name)), [], name)

    def test_the_handset_is_one_board_among_several(self):
        handset, qemu, qemu64 = (boards.load(n) for n in ("xt2163dl", "qemu", "qemu64"))
        self.assertEqual((handset["arch"], qemu["arch"], qemu64["arch"]), ("aarch32", "aarch32", "aarch64"))
        # The same kernel platform and memory reservations on all three.
        self.assertEqual(handset["memory"]["kernel_ram"], qemu["memory"]["kernel_ram"])
        self.assertEqual(handset["memory"]["kernel_ram"], qemu64["memory"]["kernel_ram"])
        # Only the handset has no emulator; only qemu64 says what it lacks.
        self.assertNotIn("emulator", handset)
        self.assertIn("storage", qemu64["absent"])
        self.assertEqual(boards.number(handset["devices"]["storage"]["pa"], "pa"), 0x11230000)
        self.assertEqual(boards.number(qemu["devices"]["storage"]["pa"], "pa"), 0x0a000000)

    def test_validation_refuses_inconsistent_descriptions(self):
        good = boards.load("qemu64")
        def broken(change):
            board = copy.deepcopy(good)
            change(board)
            with self.assertRaises(boards.BoardError):
                boards.validate(board)
        broken(lambda b: b.update(arch="riscv64"))
        broken(lambda b: b.pop("memory"))
        broken(lambda b: b["memory"]["regions"]["root"].update(pa="0x40000000"))          # overlaps the kernel
        broken(lambda b: b["memory"]["regions"]["root"].update(pa="0x40400800"))          # not page aligned
        broken(lambda b: b["memory"]["regions"]["root"].update(pa="0x90000000"))          # outside RAM
        broken(lambda b: b["memory"]["regions"]["root"].update(size="0"))
        broken(lambda b: b["memory"]["regions"]["staging"].update(pa="0x7f000000", size="0x02000000"))  # runs past RAM's end
        broken(lambda b: b["memory"]["kernel_ram"][1].update(pa="0x40800000"))            # overlaps the region before
        broken(lambda b: b["memory"]["kernel_ram"][2].update(size="0x80000000"))          # past the end of RAM
        broken(lambda b: b["devices"]["uart"].update(pa="0x08000000"))                    # two devices at one address
        broken(lambda b: b["devices"]["uart"].update(pa="zero"))
        broken(lambda b: b["devices"]["uart"].update(windows=[{"owner": "x", "va": "0xfffff000", "rights": "rw"}], size="0x2000"))
        broken(lambda b: b["absent"].append("uart"))
        # A window without rights, or larger than its device.
        broken(lambda b: b["devices"]["uart"].update(windows=[{"owner": "x", "va": "0x09000000"}]))
        broken(lambda b: b["devices"]["uart"].update(windows=[{"owner": "x", "va": "0x09000000", "rights": "rwx"}]))
        broken(lambda b: b["devices"]["uart"].update(windows=[{"owner": "x", "va": "0x09000000", "rights": "r", "frames": 2}]))
        broken(lambda b: b.update(supervisor_windows=[{"owner": "x", "pa": "0x1000", "va": "0x1000", "bits": 12}]))
        with self.assertRaises(boards.BoardError):
            boards.validate(good, name="qemu")
        # A 32-bit board cannot place an image above 4 GiB.
        handset = copy.deepcopy(boards.load("xt2163dl"))
        handset["memory"]["regions"]["root"]["va"] = "0xfff00000"
        with self.assertRaises(boards.BoardError):
            boards.validate(handset)
        boards.validate(good)

    def test_generated_sources_carry_the_board_facts(self):
        rust = boards.rust(boards.load("qemu64"))
        self.assertIn('pub const ARCH: &str = "aarch64";', rust)
        self.assertIn("pub const UART_PA: u64 = 0x9000000;", rust)
        self.assertIn("pub const ROOT_VA: u64 = 0x100000;", rust)
        self.assertIn("pub const TIMER_HZ: u64 = 0x3b9aca0;", rust)
        self.assertEqual(rust, boards.rust(boards.load("qemu64")), "stable output")
        header = boards.c_header(boards.load("xt2163dl"))
        self.assertIn("#define BOARD_STORAGE_PA 0x11230000ull", header)
        self.assertIn("#define BOARD_TIMER_HZ 0xc65d40ull", header)
        self.assertIn("#define BOARD_FRAMEBUFFER_PA 0x79450000ull", header)

    def test_emulator_command_comes_from_the_board(self):
        command = boards.emulator(boards.load("qemu64"), [("a,b.bin", 0x48000000)], 0x48000000)
        self.assertEqual(command[:7], ["qemu-system-aarch64", "-machine", "virt,gic-version=3", "-cpu", "cortex-a53", "-smp", "1"])
        self.assertIn("loader,file=a,,b.bin,addr=0x48000000,force-raw=on", command)
        self.assertIn("loader,addr=0x48000000,cpu-num=0", command)
        with self.assertRaises(boards.BoardError):
            boards.emulator(boards.load("xt2163dl"))

    def test_architecture_table(self):
        a32, a64 = boards.ARCHES["aarch32"], boards.ARCHES["aarch64"]
        # The 32-bit entries are the values the default build has always used.
        self.assertEqual((a32["clang_triple"], a32["rust_target"], a32["app_rust_target"]),
                         ("arm-none-eabi", "armv7a-none-eabi", "armv7-unknown-linux-musleabihf"))
        self.assertEqual((a64["clang_triple"], a64["rust_target"], a64["elf_class"], a64["elf_machine"]),
                         ("aarch64-none-elf", "aarch64-unknown-none", 2, 183))
        self.assertEqual(set(a32), set(a64))


SOURCE = """
struct device_page devices[] = {
    {TOUCH, 0x09000000, 0x09003000, 0},   {RADIO, 0x09010000, 0x09004000, 0},
    // {POWER, 0x0dead000, 0x09000000, 0},  a commented-out row
    {0, 0x0a003000, 0x0a003000, 0}, {OPEN_RT, 0x49230000, 0x09400000, 0, 16}};
#ifdef VIOLET_QEMU
  device(0, 0x49300000, 0x09100000, 12);
#else
  board_device(TOUCH, 0x10007000, 0x09005000, 0x09703000);
  device(RADIO,0x18006000,0x09014000,12); // WLAN
  for (unsigned off = 0; off < 0x2000; off += PAGE)
      map_device(0x11220000 + off, 0x09010000 + off, 12);
  map_device_rights(0x10010000, 0x09000000, 12, seL4_CanRead);
#endif
#ifdef VIOLET_MOTO
  device(0, 0x79450000, 0x07900000, 16);
#endif
  device(0, 0x12345000, 0x09000000, 12);
"""


class Supervisor(unittest.TestCase):
    def test_windows_are_read_from_each_boards_part_of_the_source(self):
        windows = boards.supervisor_windows(SOURCE)
        key = lambda pa, owner, va, bits=12, frames=1, rights="rw": (pa, owner, va, bits, frames, rights)
        # QEMU: its own calls, the unconditional one, then the devices[] rows.
        self.assertEqual([boards.window_key(w) for w in windows["qemu"]], [
            key(0x49300000, "supervisor", 0x09100000), key(0x12345000, "supervisor", 0x09000000),
            key(0x09000000, "touch", 0x09003000), key(0x09010000, "radio", 0x09004000),
            key(0x0a003000, "supervisor", 0x0a003000), key(0x49230000, "open_rt", 0x09400000, 16)])
        # The handset: board_device() is two windows (the process's and the
        # supervisor's), a loop is its whole extent, read-only stays read-only.
        self.assertEqual([boards.window_key(w) for w in windows["xt2163dl"]], [
            key(0x10007000, "touch", 0x09005000), key(0x10007000, "supervisor", 0x09703000),
            key(0x18006000, "radio", 0x09014000), key(0x11220000, "context", 0x09010000, frames=2),
            key(0x10010000, "context", 0x09000000, rights="r"),
            key(0x79450000, "supervisor", 0x07900000, 16), key(0x12345000, "supervisor", 0x09000000)])

    def test_preprocessor_conditions_and_loop_bounds_are_followed(self):
        text = """
#if defined(VIOLET_QEMU) && !defined(VIOLET_MOTO)
  device(0, 0x49300000, 0x09100000, 12);
#elif defined(VIOLET_MOTO)
  device(0, 0x43c00000, 0x0d000000, 20);
#else
  device(0, 0x11111000, 0x09000000, 12);
#endif
#ifdef VIOLET_DEVELOPER_KEY
  device(RADIO, 0x18004000, 0x09011000, 12);
#endif
  unsigned first = 0;
#ifdef VIOLET_MOTO
  first = 65536;
#endif
  for (unsigned off = first; off < 720 * 1600 * 4; off += 65536) {
    device(DISPLAY, 0x79450000 + off, 0x08000000 + off, 16);
  }
"""
        windows = boards.supervisor_windows(text)
        qemu = {boards.window_key(w) for w in windows["qemu"]}
        moto = {boards.window_key(w) for w in windows["xt2163dl"]}
        self.assertIn((0x49300000, "supervisor", 0x09100000, 12, 1, "rw"), qemu)
        self.assertNotIn((0x43c00000, "supervisor", 0x0d000000, 20, 1, "rw"), qemu)
        self.assertIn((0x43c00000, "supervisor", 0x0d000000, 20, 1, "rw"), moto)
        self.assertFalse(any(k[0] == 0x11111000 for k in qemu | moto), "the #else neither board compiles")
        # A build option either way: a window some build of the board maps.
        self.assertIn((0x18004000, "radio", 0x09011000, 12, 1, "rw"), qemu & moto)
        # The display loop on both boards, from the start each one assigns.
        self.assertIn((0x79450000, "display", 0x08000000, 16, 71, "rw"), qemu)
        self.assertIn((0x79460000, "display", 0x08010000, 16, 70, "rw"), moto)

    def test_a_window_whose_extent_or_rights_cannot_be_read_is_an_error(self):
        for text in ["  device(DISPLAY, 0x79450000 + off, 0x08000000 + off, 16);\n",
                     "  for (unsigned off = 0; off < limit; off += PAGE)\n    map_device(0x11220000 + off, 0x09010000 + off, 12);\n",
                     "  for (unsigned off = 0; off < 0x4000; off += 0x2000)\n    map_device(0x11220000 + off, 0x09010000 + off, 12);\n",
                     "  map_device_rights(0x10010000, 0x09000000, 12, seL4_CanWrite);\n"]:
            with self.assertRaises(boards.BoardError, msg=text):
                boards.supervisor_windows(text)

    def test_a_window_added_or_dropped_in_the_code_is_reported(self):
        board = {"name": "xt2163dl", "devices": {
            "watchdog": {"pa": "0x10007000", "size": "0x1000", "windows": [{"owner": "touch", "va": "0x09005000", "rights": "rw"}]}},
            "supervisor_windows": [{"owner": "supervisor", "pa": "0x10007000", "va": "0x09703000", "bits": 12, "rights": "rw"},
                                   {"owner": "radio", "pa": "0x18006000", "va": "0x09014000", "bits": 12, "rights": "rw"},
                                   {"owner": "context", "pa": "0x11220000", "va": "0x09010000", "bits": 12, "frames": 2, "rights": "rw"},
                                   {"owner": "context", "pa": "0x10010000", "va": "0x09000000", "bits": 12, "rights": "r"},
                                   {"owner": "supervisor", "pa": "0x79450000", "va": "0x07900000", "bits": 16, "rights": "rw"},
                                   {"owner": "supervisor", "pa": "0x12345000", "va": "0x09000000", "bits": 12, "rights": "rw"}]}
        self.assertEqual(boards.drift(board, SOURCE), [])
        more = SOURCE.replace("#ifdef VIOLET_MOTO\n", "#ifdef VIOLET_MOTO\n  device(RADIO, 0x18009000, 0x09016000, 12);\n")
        self.assertEqual(len(boards.drift(board, more)), 1)
        self.assertIn("0x18009000", boards.drift(board, more)[0])
        moved = SOURCE.replace("0x09014000", "0x09019000")
        self.assertEqual(len(boards.drift(board, moved)), 2, "the new address and the stale one")
        # A read-only window made writable, a loop made longer, an unconditional window moved.
        writable = SOURCE.replace("map_device_rights(0x10010000, 0x09000000, 12, seL4_CanRead)", "map_device(0x10010000, 0x09000000, 12)")
        self.assertEqual(len(boards.drift(board, writable)), 2, boards.drift(board, writable))
        longer = SOURCE.replace("off < 0x2000", "off < 0x4000")
        self.assertEqual(len(boards.drift(board, longer)), 2)
        unconditional = SOURCE.replace("0x12345000", "0x12346000")
        self.assertEqual(len(boards.drift(board, unconditional)), 2)
        board["devices"]["watchdog"]["windows"][0]["owner"] = "power"
        self.assertEqual(len(boards.drift(board, SOURCE)), 2, "a window given to another process")

    def test_the_live_supervisor_cannot_change_a_window_unnoticed(self):
        # The three edits a review made to supervisor.c, each of which the
        # check used to miss, and the read-only image window made writable.
        live = boards.SUPERVISOR.read_text()
        edits = [("device(DISPLAY, 0x79450000 + off", "device(DISPLAY, 0x79460000 + off", ("qemu", "xt2163dl")),
                 ("off < 0xa000; off += PAGE)\n      map_device(0x11220000", "off < 0x40000; off += PAGE)\n      map_device(0x11220000", ("xt2163dl",)),
                 ("report = device(0, 0x49300000", "report = device(0, 0x49310000", ("qemu",)),
                 ("map_device_rights(0x4a000000 + off, 0x10000000 + off, 24, seL4_CanRead)",
                  "map_device(0x4a000000 + off, 0x10000000 + off, 24)", ("qemu",))]
        for old, new, affected in edits:
            self.assertIn(old, live, "supervisor.c changed: update this test")
            text = live.replace(old, new)
            for name in ("qemu", "xt2163dl"):
                errors = boards.drift(boards.load(name), text)
                if name in affected:
                    self.assertTrue(errors, f"{name} missed: {new}")
                else:
                    self.assertEqual(errors, [], name)

    def test_a_board_fact_changed_in_the_code_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            for _, _, file, _, _ in boards.FACTS:
                (repo / file).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(boards.REPO / file, repo / file)
            shutil.copyfile(boards.REPO / "platform/sel4/board/moto-radio-memory.dts", repo / "platform/sel4/board/moto-radio-memory.dts")
            handset = boards.load("xt2163dl")
            self.assertEqual(boards.fact_errors(handset, repo), [])
            loader = repo / "boot/loader/loader.c"
            loader.write_text(loader.read_text().replace("fb = 0x79450000", "fb = 0x79460000"))
            errors = boards.fact_errors(handset, repo)
            self.assertEqual(len(errors), 1)
            self.assertIn("0x79460000", errors[0])
            # A fact the check can no longer find is an error too, not a silent pass.
            loader.write_text(loader.read_text().replace("open_base", "apps_base"))
            self.assertEqual(len(boards.fact_errors(handset, repo)), 2)
            dts = repo / "platform/sel4/board/moto-radio-memory.dts"
            dts.write_text(dts.read_text().replace("0x00d00000", "0x00c00000"))
            self.assertTrue(any("kernel_ram" in e for e in boards.fact_errors(boards.load("qemu64"), repo)))


if __name__ == "__main__":
    unittest.main()
