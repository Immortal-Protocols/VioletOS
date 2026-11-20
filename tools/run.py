"""Launch a fresh machine with software translation on any QEMU host."""
from pathlib import Path
import argparse
import subprocess

build = Path(__file__).resolve().parents[1] / "build"
parser = argparse.ArgumentParser()
parser.add_argument("--headless", action="store_true", help="serial logs only")
args = parser.parse_args()
command = ["qemu-system-aarch64", "-machine", "virt,accel=tcg", "-cpu", "cortex-a53",
           "-m", "128M", "-smp", "1", "-kernel", str(build/"Image"),
           "-initrd", str(build/"violet-aarch64.cpio.gz"),
           "-append", "console=ttyAMA0 loglevel=4 rdinit=/init",
           "-device", "virtio-gpu-pci,xres=480,yres=800",
           "-device", "virtio-tablet-pci", "-nic", "none", "-serial", "stdio",
           "-monitor", "none", "-no-reboot"]
if args.headless:
    command += ["-display", "none"]
try:
    raise SystemExit(subprocess.call(command))
except KeyboardInterrupt:
    pass
