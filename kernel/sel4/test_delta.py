"""Host tests for the kernel delta manifest (make test-unit): milliseconds, no kernel tree needed."""
import copy
import hashlib
import io
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import delta


def sha(data):
    return hashlib.sha256(data).hexdigest()


class Repository(unittest.TestCase):
    def test_repository_matches_the_manifest(self):
        # The patch, the board files, the build options and the upstream pin
        # are exactly what delta.json records.
        self.assertEqual(delta.source_errors(delta.load()), [])

    def test_manifest_makes_no_verification_claim(self):
        manifest = delta.load()
        self.assertIs(manifest["verified_configuration"], False)
        self.assertEqual(manifest["options"]["KernelVerificationBuild"]["value"], "OFF")
        readme = (HERE / "README.md").read_text()
        for path in [*manifest["changed"], *manifest["added"]]:
            self.assertIn(path, readme, f"README.md does not mention {path}")
        for option, entry in manifest["options"].items():
            self.assertIn(f"| `{option}` | {entry['value']} | {entry['why']} |", readme,
                          f"README.md's row for {option} does not match delta.json")
        self.assertIn("not a verified", readme)

    def test_a_changed_source_or_option_is_reported(self):
        manifest = delta.load()
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            for path in [*manifest["sources"], *manifest["not_in_kernel"], "kernel/sel4/build.py"]:
                (repo / path).parent.mkdir(parents=True, exist_ok=True)
                (repo / path).write_bytes((delta.REPO / path).read_bytes())
            self.assertEqual(delta.source_errors(manifest, repo), [])
            patch = repo / "platform/sel4/board/wifi/aarch32.patch"
            original = patch.read_text()
            # One more hunk in a file the manifest does not list.
            patch.write_text(original + "--- a/src/kernel/thread.c\n+++ b/src/kernel/thread.c\n@@ -1,1 +1,2 @@\n+// x\n")
            errors = delta.source_errors(manifest, repo)
            self.assertTrue(any("changed since" in e and "aarch32.patch" in e for e in errors), errors)
            self.assertTrue(any("src/kernel/thread.c" in e for e in errors), errors)
            patch.write_text(original)
            # A new board file nobody classified.
            (repo / "platform/sel4/board/wifi/extra.S").write_text("smc #0\n")
            self.assertTrue(any("extra.S" in e for e in delta.source_errors(manifest, repo)))
            (repo / "platform/sel4/board/wifi/extra.S").unlink()
            # A new kernel option, and a different upstream pin.
            script = repo / "kernel/sel4/build.py"
            text = script.read_text()
            script.write_text(text.replace('"-DKernelPrinting=OFF"', '"-DKernelPrinting=OFF","-DKernelAllowSMCCalls=ON"'))
            self.assertTrue(any("KernelAllowSMCCalls" in e for e in delta.source_errors(manifest, repo)))
            # The reviewers' case: an option's value changes, its name does not.
            for old, new, word in [
                ('"-DKernelDebugBuild=OFF"', '"-DKernelDebugBuild=ON"', "KernelDebugBuild is ON"),
                ('"-DKernelPrinting=OFF"', '"-DKernelPrinting=ON"', "KernelPrinting is ON"),
                ('os.environ.get("VIOLET_MCS")=="1"', 'os.environ.get("VIOLET_MCS")!="0"', "KernelIsMCS is"),
                ('os.environ.get("VIOLET_CORES","1")', 'os.environ.get("VIOLET_CORES","4")', "KernelMaxNumNodes is"),
                ('"-DKernelPrinting=OFF"', '"-DKernelPrinting="+os.environ.get("P","OFF")', "KernelPrinting: build.py sets a expression"),
                ('"-DKernelArmTLSReg=tpidruro"', '"-DKernelArmTLSReg=tpidruro","-DKernelDebugBuild=ON"', "KernelDebugBuild is set twice"),
                ('"-DKernelArmTLSReg=tpidruro"', '"-DKernelArmTLSReg=tpidruro",f"-DKernelDebugBuild={target}"', "cannot read: '-DKernelDebugBuild='"),
                ('"-DKernelArmTLSReg=tpidruro"', '"-DKernelArmTLSReg=tpidruro","-DCMAKE_BUILD_TYPE=Debug"', "CMAKE_BUILD_TYPE"),
            ]:
                self.assertIn(old, text)
                script.write_text(text.replace(old, new))
                errors = delta.source_errors(manifest, repo)
                self.assertTrue(any(word in e for e in errors), (new, errors))
            script.write_text(text.replace(manifest["upstream"]["sha256"], "0" * 64))
            self.assertTrue(any("does not pin" in e for e in delta.source_errors(manifest, repo)))
            script.write_text(text)
            (repo / "platform/sel4/board/config.cmake").unlink()
            self.assertTrue(any("missing" in e for e in delta.source_errors(manifest, repo)))


class Tree(unittest.TestCase):
    MANIFEST = {"changed": {"src/a.c": {}}, "added": {"src/plat/new.S": {}}}

    def pristine(self):
        return {"src/a.c": sha(b"a"), "src/b.c": sha(b"b"), "tools/t.py": sha(b"t"), "inc": "link:src"}

    def tree(self):
        return {**self.pristine(), "src/a.c": sha(b"a patched"), "src/plat/new.S": sha(b"new")}

    def test_exact_delta_passes(self):
        self.assertEqual(delta.tree_errors(self.MANIFEST, self.pristine(), self.tree()), [])

    def test_every_unlisted_difference_is_reported(self):
        cases = {
            "upstream file changed but not listed: src/b.c": {**self.tree(), "src/b.c": sha(b"b backdoor")},
            "file added to the kernel tree but not listed: src/evil.c": {**self.tree(), "src/evil.c": sha(b"x")},
            "listed as changed but identical to upstream (or missing): src/a.c": {**self.tree(), "src/a.c": sha(b"a")},
            "upstream file changed but not listed: inc": {**self.tree(), "inc": "link:elsewhere"},
        }
        for message, tree in cases.items():
            self.assertEqual(delta.tree_errors(self.MANIFEST, self.pristine(), tree), [message])
        missing = self.tree()
        del missing["src/plat/new.S"], missing["tools/t.py"]
        self.assertEqual(delta.tree_errors(self.MANIFEST, self.pristine(), missing),
                         ["listed as added but absent (or present upstream): src/plat/new.S", "upstream file removed: tools/t.py"])

    def test_archive_and_tree_hashing_agree(self):
        with tempfile.TemporaryDirectory() as directory:
            d = Path(directory)
            archive = d / "up.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                for name, data in [("seL4-x/src/a.c", b"a"), ("seL4-x/src/b.c", b"b"), ("seL4-x/tools/t.py", b"t")]:
                    info = tarfile.TarInfo(name)
                    info.size = len(data)
                    tar.addfile(info, io.BytesIO(data))
                link = tarfile.TarInfo("seL4-x/inc")
                link.type, link.linkname = tarfile.SYMTYPE, "src"
                tar.addfile(link)
            pristine = delta.archive_hashes(archive)
            self.assertEqual(pristine, self.pristine())
            tree = d / "tree"
            with tarfile.open(archive) as tar:
                tar.extractall(d)
            (d / "seL4-x").rename(tree)
            self.assertEqual(delta.tree_hashes(tree), pristine)
            (tree / "src/a.c").write_bytes(b"a patched")
            (tree / "src/plat").mkdir()
            (tree / "src/plat/new.S").write_bytes(b"new")
            # Byte-code caches from the kernel's Python tools are not kernel source.
            (tree / "tools/__pycache__").mkdir()
            (tree / "tools/__pycache__/t.cpython-312.pyc").write_bytes(b"cache")
            self.assertEqual(delta.tree_errors(self.MANIFEST, pristine, delta.tree_hashes(tree)), [])
            (tree / "src/a.c.orig").write_bytes(b"a")
            self.assertEqual(delta.tree_errors(self.MANIFEST, pristine, delta.tree_hashes(tree)),
                             ["file added to the kernel tree but not listed: src/a.c.orig"])


class Content(unittest.TestCase):
    """A listed file must hold exactly what its patch, script or source produces."""

    UPSTREAM = {"src/a.c": b"one\ntwo\nthree\n", "src/b.c": b"b\n", "tools/rules.yml": b"rule: 1\n",
                "include/board/constants.h": b"#define N 1\n"}
    PATCH = "--- a/src/a.c\n+++ b/src/a.c\n@@ -1,3 +1,4 @@\n one\n+patched\n two\n three\n"
    SCRIPT = ("import sys, pathlib\np = pathlib.Path(sys.argv[1]) / 'tools/rules.yml'\n"
              "t = p.read_text()\nif 'ours' not in t: p.write_text(t + 'rule: ours\\n')\n")

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        d = Path(self.scratch.name)
        self.repo, self.tree, self.archive = d / "repo", d / "tree", d / "up.tar.gz"
        with tarfile.open(self.archive, "w:gz") as tar:
            for name, data in self.UPSTREAM.items():
                info = tarfile.TarInfo("seL4-x/" + name)
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
        (self.repo / "board").mkdir(parents=True)
        (self.repo / "board/a.patch").write_text(self.PATCH)
        (self.repo / "board/rules.py").write_text(self.SCRIPT)
        (self.repo / "board/new.S").write_text("stub\n")
        self.manifest = {
            "changed": {"src/a.c": {"from": "board/a.patch"}, "tools/rules.yml": {"from": "board/rules.py"}},
            "added": {"src/plat/new.S": {"from": "board/new.S"},
                      "include/ours/constants.h": {"copy_of": "include/board/constants.h"},
                      "src/plat/base.dts": {"from": "board/new.S", "handset": {"factory_boot_sha256": "f" * 64}}},
        }
        # The tree as the build prepares it.
        with tarfile.open(self.archive) as tar:
            tar.extractall(d)
        (d / "seL4-x").rename(self.tree)
        (self.tree / "src/a.c").write_bytes(b"one\npatched\ntwo\nthree\n")
        (self.tree / "tools/rules.yml").write_bytes(b"rule: 1\nrule: ours\n")
        for name, data in [("src/plat/new.S", b"stub\n"), ("src/plat/base.dts", b"stub\n"),
                           ("include/ours/constants.h", self.UPSTREAM["include/board/constants.h"])]:
            (self.tree / name).parent.mkdir(parents=True, exist_ok=True)
            (self.tree / name).write_bytes(data)

    def tearDown(self):
        self.scratch.cleanup()

    def errors(self, derived=None):
        expected = delta.expected_hashes(self.manifest, self.archive, self.repo, derived)
        return delta.tree_errors(self.manifest, delta.archive_hashes(self.archive), delta.tree_hashes(self.tree), expected)

    def test_the_prepared_tree_passes(self):
        self.assertEqual(self.errors(), [])
        # Without the content check, the same call as before.
        self.assertEqual(delta.tree_errors(self.manifest, delta.archive_hashes(self.archive), delta.tree_hashes(self.tree)), [])

    def test_an_extra_edit_in_a_listed_file_is_reported(self):
        # The reviewers' cases: one more hunk in a patched file, and a hand
        # edit in a file the idempotent script would leave alone. Both files
        # are "listed as changed", which used to be enough.
        cases = [
            ("src/a.c", b"one\npatched\ntwo\nthree\nbackdoor\n", "board/a.patch"),
            ("tools/rules.yml", b"rule: 1\nrule: ours\nrule: theirs\n", "board/rules.py"),
            ("src/plat/new.S", b"stub\nsmc #0\n", "board/new.S"),
            ("include/ours/constants.h", b"#define N 2\n", "include/board/constants.h"),
            ("src/plat/base.dts", b"other\n", "board/new.S"),
        ]
        for path, content, origin in cases:
            good = (self.tree / path).read_bytes()
            (self.tree / path).write_bytes(content)
            listed = "changed" if path in self.manifest["changed"] else "added"
            self.assertEqual(self.errors(), [f"listed as {listed} but its content is not what {origin} produces: {path}"])
            # The old check saw nothing wrong with any of them.
            self.assertEqual(delta.tree_errors(self.manifest, delta.archive_hashes(self.archive), delta.tree_hashes(self.tree)), [])
            (self.tree / path).write_bytes(good)
        self.assertEqual(self.errors(), [])

    def test_a_derived_file_is_what_the_build_says_and_nothing_else(self):
        (self.tree / "src/plat/base.dts").write_bytes(b"from the factory image\n")
        self.assertEqual(len(self.errors()), 1)
        self.assertEqual(self.errors({"src/plat/base.dts": sha(b"from the factory image\n")}), [])
        self.assertEqual(len(self.errors({"src/plat/base.dts": sha(b"something else\n")})), 1)
        # An entry that names no origin at all cannot be checked, and says so.
        self.manifest["added"]["src/plat/new.S"] = {}
        self.assertEqual(self.errors({"src/plat/base.dts": sha(b"from the factory image\n")}),
                         ["listed as added but nothing says what it must contain: src/plat/new.S"])

    def test_a_patch_that_no_longer_applies_stops_the_check(self):
        (self.repo / "board/a.patch").write_text(self.PATCH.replace(" two", " 2"))
        with self.assertRaisesRegex(SystemExit, "does not apply"):
            self.errors()

    def test_the_handset_base_tree_needs_the_pinned_factory_image(self):
        manifest = delta.load()
        path, pin = next((p, e["handset"]["factory_boot_sha256"]) for p, e in manifest["added"].items() if e.get("handset"))
        self.assertEqual(delta.handset_base(pin, b"dts"), {path: sha(b"dts")})
        with self.assertRaisesRegex(SystemExit, "not the one"):
            delta.handset_base("0" * 64, b"dts")
        # The loader requires the same image.
        self.assertIn(pin, (delta.REPO / "platform/sel4/loader/build.py").read_text())

    def test_every_real_entry_says_what_it_must_contain(self):
        manifest = delta.load()
        for path, entry in manifest["changed"].items():
            self.assertIn(entry.get("from"), manifest["sources"], path)
        for path, entry in manifest["added"].items():
            self.assertTrue(entry.get("from") in manifest["sources"] or entry.get("copy_of"), path)


class Smc(unittest.TestCase):
    LISTING = """
e0014238 <moto_emi_call>:
e0014238: e92d40f0     \tpush\t{r4, r5, r6, r7, lr}
e0014258: e1600070     \tsmc\t#0x0
e001425c: e8bd80f0     \tpop\t{r4, r5, r6, r7, pc}
e0014260: 8200050b     \t.word\t0x8200050b

e0014300 <handleSyscall>:
e0014300: e92d4800     \tpush\t{r11, lr}
"""

    def test_listed_stubs_pass_and_any_other_site_fails(self):
        manifest = copy.deepcopy(delta.load())
        self.assertEqual(delta.smc_sites(self.LISTING), {"moto_emi_call"})
        self.assertEqual(delta.smc_errors(manifest, self.LISTING), [])
        elsewhere = self.LISTING + "e0014304: e1600070     \tsmc\t#0x0\n"
        self.assertEqual(delta.smc_errors(manifest, elsewhere), ["SMC instruction outside the listed stubs: in handleSyscall"])
        manifest["smc"]["functions"] = []
        self.assertEqual(delta.smc_errors(manifest, self.LISTING), ["SMC instruction outside the listed stubs: in moto_emi_call"])


if __name__ == "__main__":
    unittest.main()
