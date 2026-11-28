"""Build a VioletOS seL4 target. AArch64 is the default architecture.

    python3 tools/build_sel4.py qemu          the VioletOS system for AArch64 QEMU (targets/qemu64:
                                              the AArch64 kernel, then stack.py; build/sel4/qemu-aarch64)
    python3 tools/build_sel4.py moto          the handset in AArch64 (targets/moto-g-pure/build64.py;
                                              build/sel4/moto-aarch64/boot.img)
    python3 tools/build_sel4.py qemu32|moto32 the 32-bit (AArch32) system (kernel/sel4/build.py qemu|moto;
                                              build/sel4/qemu, build/sel4/moto)
    python3 tools/build_sel4.py qemu64        the AArch64 kernel and bring-up image alone (kernel/sel4/build.py)

qemu-aarch64 and moto-aarch64 are the older names of qemu and moto. The
32-bit targets run with VIOLET_ARCH=aarch32 (image/packages/violet_arch.py,
whose default is aarch64), and refuse VIOLET_ARCH=aarch64.
"""
from pathlib import Path
import os
import runpy
import sys


def run32(root, target):
    if os.environ.get("VIOLET_ARCH", "aarch32") not in ("", "aarch32"):
        sys.exit(f"{target}32 builds the 32-bit system; unset VIOLET_ARCH (it is {os.environ['VIOLET_ARCH']})")
    os.environ["VIOLET_ARCH"] = "aarch32"
    sys.argv[1:2] = [target]
    runpy.run_path(str(root / "kernel/sel4/build.py"), run_name="__main__")


def fetch_shared(root):
    """dtc and BearSSL, pinned (kernel/sel4/build.py), which the 32-bit build fetches for itself."""
    import importlib.util
    spec = importlib.util.spec_from_file_location("violet_kernel_sources", root / "kernel/sel4/build.py")
    kernel = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(kernel)
    kernel.fetch_shared()


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[1]
    target = sys.argv[1] if len(sys.argv) > 1 else ""
    if target in ("qemu", "qemu-aarch64", "moto", "moto-aarch64"):
        fetch_shared(root)
    if target in ("qemu", "qemu-aarch64"):
        sys.argv[1:2] = []
        runpy.run_path(str(root / "targets/qemu64/stack.py"), run_name="__main__")
    elif target in ("moto", "moto-aarch64"):
        sys.argv[1:2] = []
        runpy.run_path(str(root / "targets/moto-g-pure/build64.py"), run_name="__main__")
    elif target in ("qemu32", "moto32"):
        run32(root, target[:-2])
    elif target == "qemu64":
        runpy.run_path(str(root / "kernel/sel4/build.py"), run_name="__main__")
    else:
        sys.exit(__doc__)
