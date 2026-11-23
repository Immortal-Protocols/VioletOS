"""Build the IPv4, DHCP, DNS and TCP portions of lwIP for the handset."""
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import subprocess

def objects(repo, out, common):
    root = repo / "build/sel4/lwip-STABLE-2_2_1_RELEASE/src"
    port = Path(__file__).with_name("ip")
    dest = out / "lwip"
    dest.mkdir(exist_ok=True)
    flags = [*common, "--target=armv7a-linux-androideabi21", "-I", port, "-I", root / "include"]
    files = sources(root, port)
    def compile(src):
        obj = dest / (src.stem + ".o")
        if not obj.is_file() or obj.stat().st_mtime_ns < max(p.stat().st_mtime_ns for p in
                [src, Path(__file__), port / "arch/cc.h", port / "lwipopts.h"]):
            subprocess.run([*flags, "-c", src, "-o", obj], check=True)
        return obj
    with ThreadPoolExecutor(6) as pool:
        return list(pool.map(compile, files))


def sources(root, port):
    return [root / (name + ".c") for name in """
        core/init core/def core/dns core/inet_chksum core/ip core/mem core/memp
        core/netif core/pbuf core/sys core/tcp core/tcp_in core/tcp_out core/timeouts core/udp
        core/ipv4/acd core/ipv4/dhcp core/ipv4/etharp core/ipv4/icmp core/ipv4/ip4 core/ipv4/ip4_addr
        netif/ethernet""".split()] + [port / "client.c", port / "sockets.c"]
