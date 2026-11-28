"""Host tests for the AArch64 kernel's delta manifest (make test-unit): milliseconds, no kernel tree needed."""
import copy
import importlib.util
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("kernel64_build", HERE / "build.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class Manifest(unittest.TestCase):
    def test_repository_matches_the_manifest(self):
        self.assertEqual(build.source_errors(build.load()), [])

    def test_same_upstream_and_no_verification_claim(self):
        manifest, main = build.load(), json.loads((HERE.parent / "delta.json").read_text())
        self.assertEqual(manifest["upstream"], main["upstream"])
        self.assertIs(manifest["verified_configuration"], False)
        self.assertEqual(manifest["options"]["KernelVerificationBuild"]["value"], "OFF")
        self.assertEqual(manifest["options"]["KernelSel4Arch"]["value"], "aarch64")
        # The board device trees are reviewed once: their hashes are the 32-bit manifest's.
        for path in ("platform/sel4/board/qemu.dts", "platform/sel4/board/gic-single-core.py"):
            self.assertEqual(manifest["sources"][path], main["sources"][path])
        readme = (HERE / "README.md").read_text()
        for path in [*manifest["changed"], *manifest["added"]]:
            self.assertIn(path, readme, f"README.md does not mention {path}")
        for option, entry in manifest["options"].items():
            self.assertIn(f"| `{option}` | {entry['value']} |", readme, f"README.md has no row for {option}")
        self.assertIn("not a verified", readme)

    def test_the_patch_changes_only_the_system_call_instruction(self):
        patch = (HERE / "linux-svc.patch").read_text()
        self.assertEqual(build.delta.patch_targets(patch),
                         ["libsel4/sel4_arch_include/aarch64/sel4/sel4_arch/syscalls.h", "src/arch/arm/64/traps.S"])
        added = [l[1:].strip() for l in patch.splitlines() if l.startswith("+") and not l.startswith("+++")]
        removed = [l[1:].strip() for l in patch.splitlines() if l.startswith("-") and not l.startswith("---")]
        # libsel4: seven stubs, each "svc #0" becoming "svc #1" and nothing else.
        self.assertEqual(removed, ['"svc #0"'] * 7)
        self.assertEqual(added.count('"svc #1"'), 7)
        # traps.S: seven instructions and two labels, the rest comment. An x8
        # the kernel's int would see as negative becomes 0x10000, never one of
        # seL4's own (negative) numbers.
        code = [l for l in added if l and l != '"svc #1"' and not l.startswith(("/*", "*"))]
        self.assertEqual(code, ["tst     x25, #0xffff", "b.ne    1f", "mov     x2, x8", "lsr     x9, x8, #31", "cbz     x9, 2f",
                                "mov     x2, #0x10000", "2:", "b       c_handle_syscall", "1:"])

    def test_the_timer_patch_gives_the_physical_timer_to_flagged_threads_only(self):
        # ellis-timer.patch: CNTKCTL_EL1.EL0PTEN follows the running thread's
        # seL4_TCBFlag_ellisTimer on every switch (the slow path and the IPC
        # fastpath), the root task starts with it, and only a thread that has
        # it may change it. The kernel's own tick is the virtual timer.
        patch = (HERE / "ellis-timer.patch").read_text()
        self.assertEqual(build.delta.patch_targets(patch), [
            "include/arch/arm/arch/64/mode/fastpath/fastpath.h", "libsel4/include/sel4/constants.h",
            "src/arch/arm/64/kernel/thread.c", "src/kernel/boot.c", "src/object/tcb.c"])
        self.assertNotIn("-", [l[0] for l in patch.splitlines() if l and not l.startswith("---")])
        files = {}
        for chunk in patch.split("--- a/")[1:]:
            name = chunk.split("\n", 1)[0]
            files[name] = [l[1:] for l in chunk.splitlines() if l.startswith("+") and not l.startswith("+++")]
        calls = {name: sum("ellisTimerAccess(" in l for l in lines) for name, lines in files.items()}
        self.assertEqual(calls, {"include/arch/arm/arch/64/mode/fastpath/fastpath.h": 1, "libsel4/include/sel4/constants.h": 0,
                                 "src/arch/arm/64/kernel/thread.c": 1, "src/kernel/boot.c": 1, "src/object/tcb.c": 1})
        # Every added line outside comments is inside CONFIG_ELLIS_TIMER_GRANT, except the flag's own value.
        self.assertIn("seL4_TCBFlag_ellisTimer = 0x2,", [l.strip() for l in files["libsel4/include/sel4/constants.h"]])
        self.assertIn("tcb->tcbFlags |= seL4_TCBFlag_ellisTimer;", [l.strip() for l in files["src/kernel/boot.c"]])
        tcb = "\n".join(files["src/object/tcb.c"])
        self.assertIn("!(NODE_STATE(ksCurThread)->tcbFlags & seL4_TCBFlag_ellisTimer)", tcb)
        self.assertIn("seL4_IllegalOperation", tcb)
        header = (HERE / "ellis_timer.h").read_text()
        code = [l for l in header.splitlines() if "MSR(" in l or "MRS(" in l]
        self.assertEqual([l.strip() for l in code], ['MRS("CNTKCTL_EL1", was);', 'MSR("CNTKCTL_EL1", want);'])
        self.assertIn("#define ELLIS_CNTKCTL_EL0PTEN BIT(9)", header)
        self.assertNotIn("MSR(", patch)
        self.assertIn("config_set(KernelEllisTimerGrant ELLIS_TIMER_GRANT ON)", (HERE / "config.cmake").read_text())
        archive = HERE.parents[2] / "build/sel4/downloads/seL4-17.0.0.tar.gz"
        if archive.is_file():
            text = build.delta.archive_texts(archive, ["include/arch/arm/arch/64/mode/machine.h"])[0]
            # Without hypervisor support the kernel's tick is CNTV (PPI 27), never CNTP.
            self.assertIn('#define CNT_TVAL "cntv_tval_el0"', text)
            self.assertNotIn("cntp_", text)

    def test_a_changed_source_patch_or_option_is_reported(self):
        manifest = build.load()
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            for path in manifest["sources"]:
                (repo / path).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(build.ROOT / path, repo / path)
            script = repo / "build.py"
            shutil.copyfile(HERE / "build.py", script)
            self.assertEqual(build.source_errors(manifest, repo, script), [])
            patch = repo / "kernel/sel4/aarch64/linux-svc.patch"
            original = patch.read_text()
            patch.write_text(original + "--- a/src/kernel/thread.c\n+++ b/src/kernel/thread.c\n@@ -1,1 +1,2 @@\n+// x\n")
            errors = build.source_errors(manifest, repo, script)
            self.assertTrue(any("changed since" in e for e in errors), errors)
            self.assertTrue(any("src/kernel/thread.c" in e for e in errors), errors)
            patch.write_text(original)
            (repo / "platform/sel4/board/qemu.dts").write_text("/dts-v1/; / {};\n")
            self.assertTrue(any("qemu.dts" in e for e in build.source_errors(manifest, repo, script)))
            shutil.copyfile(build.ROOT / "platform/sel4/board/qemu.dts", repo / "platform/sel4/board/qemu.dts")
            text = script.read_text()
            script.write_text(text.replace('"-DKernelDebugBuild=OFF"', '"-DKernelDebugBuild=ON"'))
            self.assertTrue(any("KernelDebugBuild" in e for e in build.source_errors(manifest, repo, script)))
            script.write_text(text.replace('"-DKernelMaxNumNodes=1",', '"-DKernelMaxNumNodes=1", "-DKernelAllowSMCCalls=ON",'))
            self.assertTrue(any("KernelAllowSMCCalls" in e for e in build.source_errors(manifest, repo, script)))
            script.write_text(text)
            wrong = copy.deepcopy(manifest)
            wrong["upstream"]["sha256"] = "0" * 64
            self.assertTrue(any("upstream" in e for e in build.source_errors(wrong, repo, script)))
            del wrong["changed"]["src/arch/arm/64/traps.S"]
            self.assertTrue(any("linux-svc.patch touches" in e for e in build.source_errors(wrong, repo, script)))

    def test_the_effective_configuration_is_checked_not_only_the_options_passed(self):
        manifest = build.load()
        near = manifest["nearest_verified"]
        # A verified configuration as upstream would set it (the real one is read
        # from the pinned archive at build time; below, when it is here).
        verified = {name: entry["verified"] for name, entry in near["differences"].items()}
        verified.update(KernelSel4Arch="aarch64", KernelFastpath="ON", KernelMaxNumNodes="1")
        cache = {**verified, **{n: e["violetos"] for n, e in near["differences"].items()}, **near["beyond"]}
        text = lambda settings: "".join(f"{k}:STRING={v}\n" for k, v in settings.items())
        self.assertEqual(build.configuration_errors(text(cache), verified, manifest), [])
        # The EL0 physical timer exported (or any listed value changed) is an error.
        self.assertTrue(any("KernelArmExportPTMRUser" in e for e in
                            build.configuration_errors(text({**cache, "KernelArmExportPTMRUser": "ON"}), verified, manifest)))
        # A default that drifted although no -D option names it.
        self.assertTrue(any("KernelFastpath" in e for e in
                            build.configuration_errors(text({**cache, "KernelFastpath": "OFF"}), verified, manifest)))
        self.assertEqual(manifest["options"]["KernelArmExportPTMRUser"]["value"], "OFF")
        self.assertIn('"-DKernelArmExportPTMRUser=OFF"', (HERE / "build.py").read_text())
        archive = HERE.parents[2] / "build/sel4/downloads/seL4-17.0.0.tar.gz"
        if archive.is_file():
            real = build.verified_settings(archive, manifest)
            self.assertEqual(real["KernelSel4Arch"], "aarch64")
            # Every recorded difference names what the real verified configuration sets.
            for name, entry in near["differences"].items():
                self.assertEqual(real.get(name), entry["verified"], name)

    def test_the_32_bit_build_script_gained_no_kernel_option(self):
        # kernel/sel4/build.py only forwards the qemu64 target: its options are still delta.json's.
        manifest = json.loads((HERE.parent / "delta.json").read_text())
        self.assertEqual(build.delta.option_errors(manifest, (HERE.parent / "build.py").read_text()), [])


if __name__ == "__main__":
    unittest.main()
