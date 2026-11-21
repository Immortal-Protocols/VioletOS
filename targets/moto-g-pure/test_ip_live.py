"""Run the Moto IP and TLS clients with a test-only Ethernet gateway."""
from pathlib import Path
import argparse
import shlex
import subprocess
from build_ip import sources

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--build-only", action="store_true")
args = parser.parse_args()
here = Path(__file__).resolve().parent
repo = here.parents[1]
build = repo / "build/sel4"
src = build / "lwip-STABLE-2_2_1_RELEASE/src"
tls = build / "bearssl-0.6"
radio = repo / "services/radio"
subprocess.run(["make", "-s", "-C", tls, "build/libbearssl.a"], check=True)
slirp = shlex.split(subprocess.check_output(["pkg-config", "--cflags", "--libs", "slirp"], text=True))
exe = build / "moto/test-ip-live"
subprocess.run(["cc", "-O2", "-I", here / "ip", "-I", src / "include", "-I", tls / "inc",
                *sources(src, here / "ip"), radio / "tls.c", here / "ip/test_link.c",
                tls / "build/libbearssl.a", *slirp, "-o", exe], check=True)
if not args.build_only:
    subprocess.run([exe], check=True, timeout=40)
