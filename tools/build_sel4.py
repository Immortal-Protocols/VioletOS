"""Compatibility entry point for building a VioletOS seL4 target."""
from pathlib import Path
import runpy

if __name__ == "__main__":
    runpy.run_path(str(Path(__file__).resolve().parents[1] / "kernel/sel4/build.py"), run_name="__main__")
