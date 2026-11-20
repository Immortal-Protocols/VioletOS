"""Time a fresh kernel build and image packaging while holding the service ELFs fixed."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

here = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("kernel_source", type=Path)
parser.add_argument("built_services", type=Path)
args = parser.parse_args()
services = args.built_services.resolve()
sdk = Path(os.environ["MICROKIT_SDK"])
run = Path(tempfile.mkdtemp(prefix="bench-", dir=services.parent))
kernel = run / "kernel"

def identities():
    return {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (services / "network.elf", services / "notes.elf")}

original = identities()
start = time.perf_counter()
with (run / "build.log").open("wb") as log:
    subprocess.run(["sh", here / "kernel.sh", args.kernel_source, kernel],
                   stdout=log, stderr=subprocess.STDOUT, check=True)
after_kernel = time.perf_counter()
with (run / "package.log").open("wb") as log:
    subprocess.run([sdk / "bin/microkit", here / "isolated.system", "--search-path", services,
                    "--board", "qemu_virt_aarch64", "--config", "debug", "--override-kernel",
                    kernel / "kernel.elf", "-r", run / "isolated.report", "-o", run / "isolated.img"],
                   stdout=log, stderr=subprocess.STDOUT, check=True)
end = time.perf_counter()
if identities() != original:
    raise SystemExit("Service binary changed")
config = json.loads((kernel / "gen_config/kernel/gen_config.json").read_text())
sdk_config = sdk / "board/qemu_virt_aarch64/debug/include/kernel/gen_config.json"
if config != json.loads(sdk_config.read_text()):
    raise SystemExit("Kernel configuration differs from SDK")
result = dict(kernel_s=after_kernel-start, packaging_s=end-after_kernel, total_s=end-start, services=original)
(run / "timing.json").write_text(json.dumps(result, indent=2))
print(run, result, flush=True)
subprocess.run(["python3", here / "boot_test.py", "isolated", run / "isolated.img"], check=True)
