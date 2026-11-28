"""Build the pinned seL4 as an AArch64 kernel for QEMU virt (the `qemu64` target).

The same upstream archive as the AArch32 kernels (../build.py), the same
`ellis` device trees, a 64-bit platform file and one patch. Nothing here is
used by, or changes, the AArch32 build. Every difference from upstream is
listed in delta.json next to this file and enforced on each build, exactly as
../delta.py does for the 32-bit kernels: the prepared tree, every build
option, and the effective configuration (CMakeCache.txt) against the nearest
verified one.

    python3 kernel/sel4/aarch64/build.py [--out DIR]   build kernel.elf and the libsel4 headers
    python3 kernel/sel4/aarch64/build.py --check       compare the repository with delta.json (host, no build)
    python3 kernel/sel4/aarch64/build.py --update      record the current source hashes
"""
import argparse
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
BOARD = ROOT / "platform/sel4/board"
MANIFEST = HERE / "delta.json"
SHARED = HERE.parent / "delta.json"


def _module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


delta = _module("violet_kernel_delta", HERE.parent / "delta.py")


def load():
    """delta.json, with each source hash written as "shared" taken from ../delta.json.

    The board device trees enter both kernels. Their reviewed hash lives in
    one place, so a change to one of them is recorded once.
    """
    manifest = json.loads(MANIFEST.read_text())
    shared = json.loads(SHARED.read_text())["sources"]
    for path, digest in manifest["sources"].items():
        if digest == "shared":
            manifest["sources"][path] = shared.get(path, "not in kernel/sel4/delta.json")
    return manifest


def source_errors(manifest, repo=ROOT, script=None):
    """Does the repository still match what delta.json (AArch64) records?"""
    repo, errors = Path(repo), []
    main = json.loads(SHARED.read_text())
    if manifest["upstream"] != main["upstream"]:
        errors.append("the AArch64 kernel does not pin the same upstream archive as kernel/sel4/delta.json")
    for path, digest in manifest["sources"].items():
        file = repo / path
        if not file.is_file():
            errors.append(f"kernel source missing: {path}")
        elif hashlib.sha256(file.read_bytes()).hexdigest() != digest:
            errors.append(f"kernel source changed since delta.json was reviewed: {path}")
    for group in ("changed", "added"):
        for path, entry in manifest[group].items():
            origin = entry.get("from")
            if origin and origin not in manifest["sources"]:
                errors.append(f"{path}: its origin {origin} is not under sources")
            if not origin and not entry.get("copy_of"):
                errors.append(f"{path}: nothing says what it must contain")
            if not entry.get("why"):
                errors.append(f"{path}: no reason given")
    for patch in [p for p in manifest["sources"] if p.endswith(".patch")]:
        if (repo / patch).is_file():
            listed = {p for p, e in manifest["changed"].items() if e.get("from") == patch}
            actual = set(delta.patch_targets((repo / patch).read_text()))
            if listed != actual:
                errors.append(f"{patch} touches {sorted(actual)}, delta.json lists {sorted(listed)}")
    text = Path(script or __file__).read_text()
    try:
        options = delta.build_options(text)
    except (SyntaxError, ValueError) as exc:
        return errors + [f"kernel/sel4/aarch64/build.py: {exc}"]
    if sorted(options) != sorted(manifest["options"]):
        errors.append(f"kernel build options {sorted(options)} differ from delta.json's {sorted(manifest['options'])}")
    for name, (kind, actual) in sorted(options.items()):
        entry = manifest["options"].get(name)
        if entry is None:
            continue
        if ("expression" in entry) != (kind == "expression"):
            errors.append(f"kernel build option {name}: build.py sets a {kind} ({actual}), delta.json records the other kind")
        elif entry.get(kind) != actual:
            errors.append(f"kernel build option {name} is {actual} in build.py, {entry.get(kind)} in delta.json")
    return errors


def prepare(src):
    """Turn a pristine upstream tree into the AArch64 kernel tree. Idempotent."""
    plat = src / "src/plat/ellis"
    plat.mkdir(exist_ok=True)
    subprocess.run([sys.executable, BOARD / "gic-single-core.py", src], check=True)
    parent = _module("violet_kernel_build", HERE.parent / "build.py")
    parent.update(plat / "config.cmake", (HERE / "config.cmake").read_bytes())
    for name in ("kernel-overlay.dts", "moto-radio-memory.dts"):
        parent.update(plat / name, (BOARD / name).read_bytes())
    parent.update(plat / "factory-base.dts", (BOARD / "qemu.dts").read_bytes())
    constants = Path("sel4/plat/api/constants.h")
    headers = src / "libsel4/sel4_plat_include"
    (headers / "ellis" / constants).parent.mkdir(parents=True, exist_ok=True)
    parent.update(headers / "ellis" / constants, (headers / "imx8mm-evk" / constants).read_bytes())
    patch = HERE / "linux-svc.patch"
    if subprocess.run(["git", "apply", "--reverse", "--check", patch], cwd=src,
                      stderr=subprocess.DEVNULL, env={**os.environ, "GIT_CEILING_DIRECTORIES": str(src.parent)}).returncode:
        subprocess.run(["git", "apply", patch], cwd=src, check=True,
                       env={**os.environ, "GIT_CEILING_DIRECTORIES": str(src.parent)})


def enforce_tree(src, archive):
    manifest = load()
    if hashlib.sha256(archive.read_bytes()).hexdigest() != manifest["upstream"]["sha256"]:
        sys.exit(f"{archive} is not the upstream archive kernel/sel4/aarch64/delta.json pins")
    errors = source_errors(manifest) + delta.tree_errors(
        manifest, delta.archive_hashes(archive), delta.tree_hashes(src), delta.expected_hashes(manifest, archive))
    if errors:
        sys.exit("Kernel delta check failed (kernel/sel4/aarch64/delta.json):\n  " + "\n  ".join(errors)
                 + f"\n  (a stale tree? delete {src} and build again)")


def configuration_errors(cache_text, verified, manifest=None):
    """Differences between a build's CMakeCache.txt and what delta.json (AArch64)
    says it is: the nearest verified configuration plus the recorded differences."""
    return delta.configuration_errors(manifest or load(), verified, delta.cache_settings(cache_text))


def verified_settings(archive, manifest=None):
    near = (manifest or load())["nearest_verified"]
    return delta.verified_settings(delta.archive_texts(archive, [*near.get("includes", []), near["config"]]))


def enforce_configuration(cache_text, archive):
    errors = configuration_errors(cache_text, verified_settings(archive))
    if errors:
        sys.exit("Kernel delta check failed (kernel/sel4/aarch64/delta.json):\n  " + "\n  ".join(errors))


def build(out):
    """Build into `out`; returns the CMake build directory (kernel.elf and libsel4 headers)."""
    parent = _module("violet_kernel_build", HERE.parent / "build.py")
    ndk = Path(os.environ.get("ANDROID_NDK_HOME", Path.home() / "Library/Android/sdk/ndk/28.2.13676358"))
    bins = list(ndk.glob("toolchains/llvm/prebuilt/*/bin/clang"))
    if not bins:
        sys.exit("Set ANDROID_NDK_HOME to the Android NDK 28.2.13676358 directory.")
    py = ROOT / "build/sel4/venv/bin/python3"
    if not py.exists():
        sys.exit("Run make setup once to install the kernel build's Python dependencies.")
    env = os.environ.copy()
    env.update(PATH=os.pathsep.join([str(py.parent), str(bins[0].parent), env["PATH"]]),
               CMAKE_POLICY_VERSION_MINIMUM="3.5")
    out.mkdir(parents=True, exist_ok=True)
    with (out / "build.lock").open("a+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        src = out / "upstream"
        parent.source(parent.KERNEL, src)
        prepare(src)
        enforce_tree(src, parent.BUILD / "downloads" / (parent.KERNEL[0] + ".tar.gz"))
        kernel = out / "kernel"
        subprocess.run([
            "cmake", "-GNinja", "-S", src, "-B", kernel, "-DCMAKE_TOOLCHAIN_FILE=" + str(src / "llvm.cmake"),
            "-DTRIPLE=aarch64-none-elf", "-DPYTHON3=" + str(py), "-DKernelPlatform=ellis",
            "-DKernelSel4Arch=aarch64", "-DKernelIsMCS=OFF", "-DKernelPrinting=OFF",
            "-DKernelDebugBuild=OFF", "-DKernelVerificationBuild=OFF",
            # User level reads the virtual counter. It may not program the
            # EL0 physical timer: the kernel does not switch it between
            # threads, so any app could cancel or move another's wake-up.
            "-DKernelArmExportVCNTUser=ON", "-DKernelArmExportPTMRUser=OFF",
            "-DKernelMaxNumNodes=1",
            # EL1 kernel: QEMU virt without virtualization starts there.
            "-DKernelArmHypervisorSupport=OFF"], env=env, check=True)
        # The effective configuration, defaults included, differs from the
        # nearest verified one exactly as delta.json says.
        enforce_configuration((kernel / "CMakeCache.txt").read_text(), parent.BUILD / "downloads" / (parent.KERNEL[0] + ".tar.gz"))
        generated = ["include/sel4/" + f for f in ["shared_types_gen.h", "syscall.h", "invocation.h"]]
        generated += ["include/interfaces/sel4_client.h", "arch_include/arm/sel4/arch/invocation.h",
                      "sel4_arch_include/aarch64/sel4/sel4_arch/invocation.h",
                      "sel4_arch_include/aarch64/sel4/sel4_arch/types_gen.h"]
        subprocess.run(["ninja", "-C", kernel, "kernel.elf", *["libsel4/" + f for f in generated]], env=env, check=True)
        # No secure-monitor call anywhere in this kernel.
        listing = subprocess.check_output([bins[0].with_name("llvm-objdump"), "-d", kernel / "kernel.elf"], text=True)
        sites = delta.smc_sites(listing)
        if sites:
            sys.exit("Kernel delta check failed: SMC instruction in " + ", ".join(sorted(map(str, sites))))
        return kernel


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, default=ROOT / "build/sel4/qemu64")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--update", action="store_true")
    args = parser.parse_args()
    if args.update:
        manifest = json.loads(MANIFEST.read_text())
        for path, digest in manifest["sources"].items():
            if digest != "shared":
                manifest["sources"][path] = hashlib.sha256((ROOT / path).read_bytes()).hexdigest()
        MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
    if args.check or args.update:
        errors = source_errors(load())
        if errors:
            sys.exit("Kernel delta check failed (kernel/sel4/aarch64/delta.json):\n  " + "\n  ".join(errors))
        print("kernel/sel4/aarch64/delta.json matches the repository")
        return
    print(build(args.out.resolve()))


if __name__ == "__main__":
    main()
