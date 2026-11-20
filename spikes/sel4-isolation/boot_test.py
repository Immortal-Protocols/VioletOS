"""Boot a local fixture, capture its serial output and check its expected behavior."""
import argparse
import os
from pathlib import Path
import re
import select
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("case", choices=("isolated", "granted"))
parser.add_argument("image", type=Path)
args = parser.parse_args()
cmd = ["qemu-system-aarch64", "-machine", "virt,virtualization=on,accel=tcg",
       "-cpu", "cortex-a53", "-m", "2G", "-smp", "1", "-display", "none",
       "-serial", "stdio", "-monitor", "none", "-nic", "none", "-device",
       f"loader,file={args.image},addr=0x70000000,cpu-num=0"]
proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                        stderr=subprocess.STDOUT)
serial = bytearray()
deadline = time.monotonic() + 20
try:
    while time.monotonic() < deadline:
        if not select.select([proc.stdout], [], [], 0.2)[0]:
            continue
        chunk = os.read(proc.stdout.fileno(), 8192)
        if not chunk:
            break
        serial.extend(chunk)
        if args.case == "granted" and b"NETWORK: observed synthetic note marker" in serial:
            break
        if args.case == "isolated" and re.search(rb"VMFault:[^\n]*0x0*40000000[^\n]*\n", serial):
            break
finally:
    proc.terminate()
    proc.wait(timeout=5)
text = serial.decode(errors="replace")
args.image.with_suffix(".serial.log").write_text(text)
for line in text.splitlines():
    if any(tag in line for tag in ("NOTES:", "NETWORK:", "VMFault:", "faulted", "received message")):
        print(line)
ready = "NOTES: initialized synthetic note" in text and "NETWORK: packet OK" in text
marker = "NETWORK: observed synthetic note marker" in text
fault = "faulting PD: Network" in text and bool(re.search(
    r"VMFault:[^\n]*fault_addr=0x0*40000000[^\n]*\(data fault\)", text))
if not ready or (marker, fault) != (args.case == "granted", args.case == "isolated"):
    raise SystemExit("Unexpected boot result. See the image's .serial.log")
