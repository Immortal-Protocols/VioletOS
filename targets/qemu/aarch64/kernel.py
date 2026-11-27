"""Build the seL4 17.0.0 QEMU Cortex-A57 kernel with a Linux SVC entry."""
import hashlib
from pathlib import Path
import shutil
import subprocess
from image import RAM_MIB


def build_kernel(source, out):
    if (source / "VERSION").read_text().strip() != "17.0.0":
        raise ValueError("this target uses seL4 17.0.0")
    kernel = out / "kernel"
    # Only the target-specific generated copy is changed. All other kernel
    # sources remain at the path supplied by the caller.
    patched = out / "kernel-source"
    patch = Path(__file__).with_name("linux-svc.patch")
    digest = hashlib.sha256(patch.read_bytes() + str(source).encode()).hexdigest()
    stamp = patched / ".violet-svc"
    if not stamp.exists() or stamp.read_text() != digest:
        shutil.copytree(source, patched, dirs_exist_ok=True, ignore=shutil.ignore_patterns(".git"))
        subprocess.run(["patch", "-p1", "-i", str(patch)], cwd=patched, check=True)
        stamp.write_text(digest)
    # The bundled ELF image consumes one initial capability per 4 KiB. Full
    # Ladybird also needs frame aliases and page tables for every helper.
    cache = kernel / "CMakeCache.txt"
    cached = cache.read_text().splitlines() if cache.exists() else []
    matching = all(any(line.startswith(key + ":") and line.endswith("=" + value) for line in cached)
                   for key, value in (("KernelRootCNodeSizeBits", "21"), ("QEMU_MEMORY", str(RAM_MIB))))
    if not (kernel / "build.ninja").exists() or not matching:
        subprocess.run(["cmake", "-GNinja", "-S", str(patched), "-B", str(kernel),
                    f"-DCMAKE_TOOLCHAIN_FILE={patched}/llvm.cmake", "-DTRIPLE=aarch64-none-elf",
                    "-DKernelPlatform=qemu-arm-virt", "-DARM_CPU=cortex-a57", "-DKernelSel4Arch=aarch64",
                    f"-DQEMU_MEMORY={RAM_MIB}", "-DKernelIsMCS=ON", "-DKernelRootCNodeSizeBits=21",
                    "-DLibSel4UseThreadLocals=OFF", "-DKernelNumDomains=16", "-DKernelNumDomainSchedules=100",
                    "-DKernelArmExportPCNTUser=ON", "-DKernelArmHypervisorSupport=ON",
                    "-DKernelArmVtimerUpdateVOffset=OFF", "-DKernelAllowSMCCalls=ON",
                    "-DKernelDebugBuild=ON", "-DKernelPrinting=ON", "-DKernelVerificationBuild=OFF",
                    "-DHardwareDebugAPI=ON", "-DKernelArmExportPTMRUser=ON"], check=True)
    subprocess.run(["cmake", "--build", str(kernel), "--parallel", "6", "--target", "kernel.elf"], check=True)
    return kernel
