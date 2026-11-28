"""What the hardware page tables can hold for open apps (make test-unit; about a second).

Two properties, followed from the Rust callers down to the page-table entry:

- The Open Runtime's own view of app memory (and its heap) is never
  executable: nothing an app wrote can run inside the runtime.
- No page of an app is ever writable and executable.

The chain has three links, and each is checked here against the shipped
source, not a copy of it:

1. Every seL4_ARM_Page_Map the Open Runtime makes is in runtime/open/sel4.c.
   Each C function there that maps a page is compiled for this machine as it
   is, with a stand-in for libsel4 whose seL4_ARM_Page_Map does what the
   kernel does with the rights and attributes (seL4 17, aarch32:
   maskVMRights, then the AP and XN bits of the entry). Every combination of
   the function's flags is run; none gives a user-writable, executable entry.
2. runtime/open/sys.rs is the only caller of those C functions; its wrappers
   pass the flags through unchanged.
3. The callers of the wrappers: the runtime's own page directory (SLOT_PD)
   is only ever mapped with `exec` false, and the one caller that can ask
   for an executable page (procmgr.rs `set`) takes `writable` and `exec` from
   two different values of one variable.

4. The supervisor (system/supervisor/supervisor.c and the headers it
   includes) maps frames straight into other processes' page directories,
   the Open Runtime's among them (shared buffers, the scanout, the camera,
   its stack and package). Every one of its seL4_ARM_Page_Map calls outside
   its own page directory (bootstrap.pd) passes ExecuteNever, traced through
   an `attrs` variable to every value it is given, except private_page's
   mapping of a package's code, which is then read-only.

procmgr.rs's decisions (which page gets which rights, through every mmap,
mprotect, fault and fork) are tested in tests/memprot_host.rs, whose stand-in
`page_map` panics on a writable and executable request. The real page tables
can only be read in the guest: see README.md, "Memory protection".

If this test fails after a change to how pages are mapped, the change needs
the same audit: the message says which link no longer matches.
"""
import itertools
import json
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SHIM = ROOT / "runtime/open/sel4.c"
SYS = ROOT / "runtime/open/sys.rs"
# Everything compiled into the Open Runtime that could map a page: its own
# sources and the ones it takes by #[path] (runtime/open/mod.rs).
SOURCES = ["runtime/open", "runtime/arch", "system/supervisor", "system/sentinel", "services/sentinel"]
# The supervisor's C, which maps pages into every process (link 4).
SUPERVISOR_C = ["system/supervisor", "platform/sel4/board"]

# libsel4 as far as mapping goes, with the kernel's side of seL4_ARM_Page_Map.
STAND_IN = r"""
#include <stdio.h>
typedef unsigned seL4_Word;
typedef struct { unsigned words[1]; } seL4_CapRights_t;
/* seL4_CapRights_new(grantReply, grant, read, write) */
#define RIGHTS(gr, g, r, w) ((seL4_CapRights_t){{(gr) << 3 | (g) << 2 | (r) << 1 | (w)}})
#define seL4_AllRights RIGHTS(1, 1, 1, 1)
#define seL4_ReadWrite RIGHTS(0, 0, 1, 1)
#define seL4_CanRead RIGHTS(0, 0, 1, 0)
#define seL4_CanWrite RIGHTS(0, 0, 0, 1)
#define seL4_NoRights RIGHTS(0, 0, 0, 0)
typedef enum {
  seL4_ARM_PageCacheable = 1, seL4_ARM_ParityEnabled = 2,
  seL4_ARM_Default_VMAttributes = 3, seL4_ARM_ExecuteNever = 4,
} seL4_ARM_VMAttributes;
enum { VMNoAccess = 0, VMKernelOnly = 1, VMReadOnly = 2, VMReadWrite = 3 };
/* The rights of the frame capability being mapped (a fresh frame is read-write). */
static int frame_rights = VMReadWrite;
static int ap, xn, cached, maps;
/* src/arch/arm/32/kernel/vspace.c: maskVMRights, then makeUserPTE (AP = the
   rights, XN = the ExecuteNever attribute, in a kernel not built as a hypervisor). */
static int seL4_ARM_Page_Map(unsigned frame, unsigned pd, unsigned va, seL4_CapRights_t rights, seL4_ARM_VMAttributes attributes) {
  int read = rights.words[0] >> 1 & 1, write = rights.words[0] & 1, vm = VMKernelOnly;
  (void)frame; (void)pd; (void)va;
  if (frame_rights == VMNoAccess) vm = VMNoAccess;
  else if (frame_rights == VMReadOnly && read) vm = VMReadOnly;
  else if (frame_rights == VMReadWrite && read) vm = write ? VMReadWrite : VMReadOnly;
  ap = vm;
  xn = (attributes & seL4_ARM_ExecuteNever) != 0;
  cached = (attributes & seL4_ARM_PageCacheable) != 0;
  maps++;
  return 0;
}
"""


def mapping_functions():
    """[(name, [parameter names], source text)] for every function in sel4.c that maps a page."""
    text = SHIM.read_text()
    found = []
    for m in re.finditer(r"^(?:int|unsigned|void) (\w+)\(([^)]*)\) \{\n.*?^\}\n", text, re.S | re.M):
        if "seL4_ARM_Page_Map" in m.group(0):
            names = [p.split()[-1].lstrip("*") for p in m.group(2).split(",")]
            found.append((m.group(1), names, m.group(0)))
    return text, found


def run_shim(functions):
    """Run every mapping function with every combination of its flags (the
    parameters after frame, pd, va), for a read-write and a read-only frame.
    Returns {(name, frame rights, flags...): (ap, xn, cached)}."""
    calls = []
    for name, names, _ in functions:
        flags = names[3:]
        for values in itertools.product((0, 1), repeat=len(flags)):
            args = ", ".join(["1", "2", "0x10000", *map(str, values)])
            label = json.dumps([name, *values])
            calls.append(f'  maps = 0; {name}({args}); printf("%s\\t%d\\t%d\\t%d\\t%d\\t%d\\n", {json.dumps(label)}, frame_rights, maps, ap, xn, cached);')
    program = (STAND_IN + "".join(text for _, _, text in functions)
               + "int main(void) {\n  for (frame_rights = VMReadOnly; frame_rights <= VMReadWrite; frame_rights++) {\n"
               + "\n".join(calls) + "\n  }\n  return 0;\n}\n")
    with tempfile.TemporaryDirectory(prefix="violet-page-rights-") as work:
        source, binary = Path(work) / "shim.c", Path(work) / "shim"
        source.write_text(program)
        subprocess.run([shutil.which("cc") or "cc", "-std=gnu11", "-O0", "-Wall", "-Werror", "-Wno-unused-function",
                        str(source), "-o", str(binary)], check=True)
        out = subprocess.run([str(binary)], check=True, capture_output=True, text=True).stdout
    table = {}
    for line in out.splitlines():
        label, frame, maps, ap, xn, cached = line.split("\t")
        assert maps == "1", f"{label}: expected exactly one seL4_ARM_Page_Map, saw {maps}"
        table[(*json.loads(label), int(frame))] = (int(ap), int(xn), int(cached))
    return table


def calls(text, name):
    """The argument lists of every call of `name` in Rust `text` (not its definition or declaration)."""
    out = []
    for m in re.finditer(r"(?<![\w.])(?:\w+::)*" + name + r"\(", text):
        line_start = text.rfind("\n", 0, m.start()) + 1
        if re.search(r"\bfn\s+$", text[line_start:m.start()] + " ") or text[line_start:m.start()].rstrip().endswith("fn"):
            continue
        depth, at, args, piece = 1, m.end(), [], ""
        while depth:
            c = text[at]
            depth += c in "([{"
            depth -= c in ")]}"
            if depth == 1 and c == ",":
                args.append(piece.strip())
                piece = ""
            elif depth:
                piece += c
            at += 1
        args.append(piece.strip())
        out.append((text.count("\n", 0, m.start()) + 1, args))
    return out


def enclosing_fn(text, line):
    """The source of the function around `line` (1-based), by its `fn` line."""
    lines = text.splitlines()
    start = next(i for i in range(line - 1, -1, -1) if re.match(r"\s*(pub(\([a-z]+\))? )?(unsafe )?fn \w+", lines[i]))
    return "\n".join(lines[start:line])


def c_function(text, line):
    """The C function around `line` (1-based): from its header (a line at
    column 0 with a parenthesis, not a declaration) to `line`."""
    lines = text.splitlines()
    start = next(i for i in range(line - 1, -1, -1)
                 if re.match(r"(?:static )?[A-Za-z_][\w\s*]*\b\w+\(", lines[i]) and not lines[i].rstrip().endswith(";"))
    end = next((i for i in range(line, len(lines)) if lines[i].startswith("}")), len(lines) - 1)
    return "\n".join(lines[start:end + 1])


def runtime_sources():
    for folder in SOURCES:
        for path in sorted((ROOT / folder).rglob("*.rs")) if (ROOT / folder).is_dir() else []:
            if "tests" not in path.relative_to(ROOT).parts and not path.name.startswith("test_"):
                yield path


USER_WRITE = lambda ap: ap == 3          # noqa: E731  AP 3: read-write at user level
USER_READ = lambda ap: ap >= 2           # noqa: E731  AP 2: read-only
USER_EXEC = lambda ap, xn: ap >= 2 and not xn   # noqa: E731  a fetch needs read access and XN clear


class PageRights(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.shim_text, cls.functions = mapping_functions()
        cls.table = run_shim(cls.functions)

    def test_link_1_every_mapping_the_shim_can_make(self):
        # All of the shim's page mappings were found and run.
        self.assertEqual(len(re.findall(r"seL4_ARM_Page_Map\(", self.shim_text)), len(self.functions),
                         "sel4.c maps a page somewhere this test does not run")
        self.assertIn("open_page_map", [name for name, _, _ in self.functions])
        for name, names, _ in self.functions:
            self.assertEqual(names[:3], ["frame", "pd", "va"], f"{name}: unexpected parameters {names}")
        for key, (ap, xn, _) in self.table.items():
            # W^X in the entry itself, whatever the caller asked for.
            self.assertFalse(USER_WRITE(ap) and USER_EXEC(ap, xn), f"{key}: a writable and executable page-table entry")
            # A read-only frame capability can only tighten the mapping.
            if key[-1] == 2:
                self.assertFalse(USER_WRITE(ap), f"{key}: a read-only frame mapped writable")

    def test_link_1_open_page_map_gives_exactly_what_is_asked(self):
        names = next(names for name, names, _ in self.functions if name == "open_page_map")
        self.assertEqual(names[3:], ["writable", "cached", "exec"], "open_page_map's flags changed: audit its callers again")
        for writable, cached, execute in itertools.product((0, 1), repeat=3):
            ap, xn, is_cached = self.table[("open_page_map", writable, cached, execute, 3)]
            self.assertTrue(USER_READ(ap))
            self.assertEqual(USER_WRITE(ap), bool(writable))
            # Executable only if asked, and never together with writable.
            self.assertEqual(USER_EXEC(ap, xn), bool(execute and not writable), (writable, cached, execute))
            self.assertEqual(bool(is_cached), bool(cached))
        # The three mappings the runtime makes: its own view of the pool,
        # an app's data, an app's code.
        self.assertEqual(self.table[("open_page_map", 1, 1, 0, 3)][:2], (3, 1), "read-write, Execute-Never")
        self.assertEqual(self.table[("open_page_map", 0, 1, 0, 3)][:2], (2, 1), "read-only, Execute-Never")
        self.assertEqual(self.table[("open_page_map", 0, 1, 1, 3)][:2], (2, 0), "read-only, executable")

    def test_link_2_sys_rs_passes_the_flags_through(self):
        text = SYS.read_text()
        seen = calls(text, "open_page_map")
        self.assertTrue(seen, "sys.rs no longer calls open_page_map: audit the new path")
        for line, args in seen:
            where = enclosing_fn(text, line)
            self.assertEqual(len(args), 6, f"sys.rs:{line}")
            writable, cached, execute = args[3:]
            if execute == "exec as i32":
                # page_map: the caller's own two booleans, cached.
                self.assertRegex(where, r"pub fn page_map\(frame: u32, pd: u32, va: u32, writable: bool, exec: bool\)", f"sys.rs:{line}")
                self.assertEqual((writable, cached), ("writable as i32", "1"), f"sys.rs:{line}")
            else:
                # Anything else (device memory) is never executable.
                self.assertEqual(execute, "0", f"sys.rs:{line}: an executable mapping this test does not know")
        # No other Rust file reaches the shim's mapping functions.
        for path in runtime_sources():
            if path != SYS:
                for name, _, _ in self.functions:
                    self.assertEqual(calls(path.read_text(), name), [], f"{path.relative_to(ROOT)} calls {name} directly")

    def test_link_3_who_asks_for_what(self):
        vma = (ROOT / "runtime/open/vma.rs").read_text()
        hw = {name: int(value) for name, value in re.findall(r"pub const (HW_\w+): u8 = (\d+);", vma)}
        self.assertNotEqual(hw["HW_RW"], hw["HW_RX"])
        own, executable = [], []
        for path in runtime_sources():
            text = path.read_text()
            rel = str(path.relative_to(ROOT))
            for line, args in calls(text, "page_map"):
                self.assertEqual(len(args), 5, f"{rel}:{line}")
                _, pd, _, writable, execute = args
                if pd == "SLOT_PD" or "SLOT_PD" in pd:
                    own.append((rel, writable, execute))
                    # The runtime's own address space: never executable.
                    self.assertEqual(execute, "false", f"{rel}:{line}: the runtime maps memory executable in its own address space")
                elif execute != "false":
                    executable.append(rel)
                    # The one caller that can ask for code: procmgr.rs `set`,
                    # where one value cannot equal both HW_RW and HW_RX.
                    self.assertEqual(rel, "system/supervisor/procmgr.rs", f"{rel}:{line}: a new caller that can map executable pages")
                    self.assertEqual((writable, execute), ("w", "x"), f"{rel}:{line}")
                    self.assertIn("let (w, x) = (hw == HW_RW, hw == HW_RX);", enclosing_fn(text, line), f"{rel}:{line}")
                    self.assertNotIn("SLOT_PD", enclosing_fn(text, line))
            for line, args in calls(text, "device_map"):
                self.assertEqual(len(args), 3, f"{rel}:{line}")
        # The view of the pool is there, writable and not executable; app code has one source.
        self.assertIn(("runtime/open/pool.rs", "true", "false"), own)
        self.assertEqual(executable, ["system/supervisor/procmgr.rs"])
        # procmgr.rs never names the runtime's page directory at all.
        self.assertNotIn("SLOT_PD", (ROOT / "system/supervisor/procmgr.rs").read_text())


    def test_link_4_the_supervisors_mappings_are_execute_never(self):
        files = [p for folder in SUPERVISOR_C for p in sorted((ROOT / folder).glob("*.[ch]"))]
        own, checked, code = 0, 0, 0
        for path in files:
            text = path.read_text()
            rel = str(path.relative_to(ROOT))
            for line, args in calls(text, "seL4_ARM_Page_Map"):
                self.assertEqual(len(args), 5, f"{rel}:{line}")
                _, pd, _, rights, attrs = (" ".join(a.split()) for a in args)
                if pd == "bootstrap.pd":
                    own += 1        # the supervisor's own address space
                    continue
                checked += 1
                function = c_function(text, line)
                if attrs == "attrs":
                    # Every value the variable gets carries ExecuteNever.
                    name = re.match(r"\s*(?:static )?\w+[\s*]+(\w+)\(", function).group(1)
                    if re.search(r"\(seL4_CPtr \w+, unsigned \w+, seL4_ARM_VMAttributes attrs\)", function.splitlines()[0]):
                        # A parameter: every caller passes ExecuteNever.
                        callers = [a for f in files for n, a in calls(f.read_text(), name)
                                   if not f.read_text().splitlines()[n - 1].startswith("static ")]   # not its definition
                        self.assertTrue(callers, f"{rel}:{line}: {name} has no callers")
                        for a in callers:
                            self.assertIn("seL4_ARM_ExecuteNever", a[-1], f"{name}({', '.join(a)}): an executable mapping")
                            self.assertNotIn("?", a[-1], f"{name}({', '.join(a)})")
                    else:
                        values = re.findall(r"\battrs\s*=\s*([^;]+);", function)
                        self.assertTrue(values, f"{rel}:{line}: where does attrs come from?")
                        for v in values:
                            self.assertIn("seL4_ARM_ExecuteNever", v, f"{rel}:{line}: attrs = {v}")
                            self.assertNotIn("?", v, f"{rel}:{line}: attrs = {v}")
                elif "?" in attrs:
                    # The one conditional: private_page, executable only for a
                    # package's code, which is then mapped read-only.
                    self.assertIn("static void private_page(", function.splitlines()[0], f"{rel}:{line}")
                    self.assertEqual(attrs, "seL4_ARM_Default_VMAttributes | (exec ? 0 : seL4_ARM_ExecuteNever)", f"{rel}:{line}")
                    self.assertEqual(rights, "seL4_CapRights_new(0, 0, 1, !exec)", f"{rel}:{line}")
                    code += 1
                else:
                    self.assertIn("seL4_ARM_ExecuteNever", attrs, f"{rel}:{line}: a mapping into another process without ExecuteNever")
        self.assertGreaterEqual(checked, 8, "the supervisor's mappings were not found")
        self.assertEqual(code, 1)
        self.assertGreaterEqual(own, 1)

    def test_link_4_the_composed_catalog_is_cached_where_it_is_shared(self):
        # The handset's installed catalog is composed by the supervisor in two
        # RAM sections and written to disk by Storage. Both must map them with
        # the same (cacheable) attributes: Storage reading them uncached saw
        # stale RAM, so the bank's checksum did not match its sectors and the
        # loader refused the catalog at the next boot (catalog_volume.rs).
        text = (ROOT / "system/supervisor/supervisor.c").read_text()
        maps = [(line, args) for name in ("seL4_ARM_Page_Map", "map_section") for line, args in calls(text, name)
                if re.search(r"\b(catalog_candidate_frame|copy_catalog)\b", args[0])]
        self.assertGreaterEqual(len(maps), 4, "the supervisor's and Storage's mappings of the composed catalog")
        self.assertTrue(any("procs[STORAGE]" in args[1] for _, args in maps), "Storage's mapping was not found")
        for line, args in maps:
            self.assertIn("seL4_ARM_Default_VMAttributes", args[4], f"supervisor.c:{line}: an uncached view of the catalog")

    def test_link_4_finds_what_it_must(self):
        # The audit itself: a share_with_open_attrs caller without ExecuteNever
        # and a plain cacheable mapping are what it reports.
        bad = "static void s(seL4_CPtr frame, unsigned va, seL4_ARM_VMAttributes attrs) {\n  check(seL4_ARM_Page_Map(copy, t->pd, va, seL4_AllRights, attrs));\n}\n"
        self.assertEqual(calls(bad + "  s(f, 0x1000, seL4_ARM_PageCacheable);\n", "s")[-1][1][-1], "seL4_ARM_PageCacheable")
        self.assertIn("static void s(", c_function(bad, 2).splitlines()[0])

if __name__ == "__main__":
    unittest.main()
