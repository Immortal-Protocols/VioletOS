"""Compatibility entry point for building a VioletOS seL4 target.

    python3 tools/build_sel4.py qemu|moto|qemu64    kernel/sel4/build.py
    python3 tools/build_sel4.py qemu-aarch64        the VioletOS system for AArch64 (targets/qemu64/stack.py)
    python3 tools/build_sel4.py moto-aarch64        the handset in AArch64 (targets/moto-g-pure/build64.py)
"""
from pathlib import Path
import runpy
import sys

if __name__ == "__main__":
    root = Path(__file__).resolve().parents[1]
    if sys.argv[1:2] == ["qemu-aarch64"]:
        sys.argv[1:2] = []
        runpy.run_path(str(root / "targets/qemu64/stack.py"), run_name="__main__")
    elif sys.argv[1:2] == ["moto-aarch64"]:
        sys.argv[1:2] = []
        runpy.run_path(str(root / "targets/moto-g-pure/build64.py"), run_name="__main__")
    else:
        runpy.run_path(str(root / "kernel/sel4/build.py"), run_name="__main__")
