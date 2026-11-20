"""Build reproducible newc ramdisks without root or host architecture tools."""
from pathlib import Path
import gzip
import hashlib
import io
import os
import stat
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
BUILD = ROOT / "build"
BASE = "https://dl-cdn.alpinelinux.org/alpine/v3.24/"
INPUTS = {
    "miniroot.tar.gz": ("releases/aarch64/alpine-minirootfs-3.24.4-aarch64.tar.gz",
                       "4b14c18f7e90a063b61462ea1462a5732c154bf6adad594974fe6487cd98a8de"),
    "miniroot-armv7.tar.gz": ("releases/armv7/alpine-minirootfs-3.24.4-armv7.tar.gz",
                             "2054061dafea7fe4cd38015701ee77119e780dd0382d0c32fad56986fe31b699"),
    "linux-virt.apk": ("main/aarch64/linux-virt-6.18.64-r0.apk",
                       "cf171f51ab44d637a7758dd5a2f6a4ecd7ddbea18d61b9e37a6d39357d2fb1d5"),
}


def source(name):
    url, digest = INPUTS[name]
    dest = BUILD / name
    if not dest.exists():
        with tempfile.NamedTemporaryFile(dir=BUILD, delete=False) as tmp:
            temp = Path(tmp.name)
        try:
            urllib.request.urlretrieve(BASE + url, temp)
            if hashlib.sha256(temp.read_bytes()).hexdigest() != digest:
                raise ValueError(f"Download checksum mismatch: {name}")
            os.replace(temp, dest)
        finally:
            temp.unlink(missing_ok=True)
    if hashlib.sha256(dest.read_bytes()).hexdigest() != digest:
        raise ValueError(f"Cached file checksum mismatch: {dest}")
    return dest


def archive(entries):
    data = bytearray()
    for ino, (name, mode, payload) in enumerate(entries + [("TRAILER!!!", 0, b"")], 1):
        fields = [ino, mode, 0, 0, 1, 0, len(payload), 0, 0, 0, 0, len(name)+1, 0]
        data.extend(("070701" + "".join(f"{v:08x}" for v in fields)).encode())
        data.extend(name.encode() + b"\0")
        data.extend(b"\0" * (-len(data) % 4))
        data.extend(payload)
        data.extend(b"\0" * (-len(data) % 4))
    return gzip.compress(data, mtime=0)


def emulator_drivers():
    # APK v2 contains multiple gzip members. Read the package tar once in memory.
    blob = gzip.decompress(source("linux-virt.apk").read_bytes())
    with tarfile.open(fileobj=io.BytesIO(blob), ignore_zeros=True) as pkg:
        prefix = next(n[:-len("modules.dep")] for n in pkg.getnames() if n.endswith("/modules.dep"))
        deps = {}
        for line in pkg.extractfile(prefix + "modules.dep").read().decode().splitlines():
            module, rest = line.split(":", 1)
            deps[module] = rest.split()
        ordered = {}
        def include(module):
            if module not in ordered:
                for dep in deps[module]:
                    include(dep)
                ordered[module] = None
        for driver in ["virtio-gpu", "virtio_input", "evdev"]:
            include(next(n for n in deps if n.endswith("/" + driver + ".ko.gz")))
        entries = [("drivers", stat.S_IFDIR | 0o755, b"")]
        names = []
        for module in ordered:
            name = Path(module).stem
            names.append(name)
            entries.append(("drivers/" + name, 0o100644, gzip.decompress(pkg.extractfile(prefix+module).read())))
        entries.append(("drivers/load", 0o100644, ("\n".join(names)+"\n").encode()))
        (BUILD / "Image").write_bytes(pkg.extractfile("boot/vmlinuz-virt").read())
        return entries


def userspace(arch, source_name, loader, init):
    entries = [(d, 0o40755, b"") for d in ["bin", "lib", "dev", "sys", "proc", "tmp"]]
    with tarfile.open(source(source_name)) as fs:
        for path in ["bin/busybox", "lib/" + loader]:
            entries.append((path, 0o100755, fs.extractfile("./"+path).read()))
    entries.append((f"lib/libc.musl-{arch}.so.1", 0o120777, loader.encode()))
    for name in ["sh", "mount", "mdev", "insmod", "sleep"]:
        entries.append(("bin/"+name, 0o120777, b"busybox"))
    entries.append(("bin/violet", 0o100755, (ROOT / "assets" / ("violet-"+arch)).read_bytes()))
    entries.append(("init", 0o100755, init.read_bytes()))
    return entries


def build(arch, source_name, loader, drivers):
    entries = userspace(arch, source_name, loader, ROOT / "guest/init")
    output = BUILD / ("violet-"+arch+".cpio.gz")
    output.write_bytes(archive(entries+drivers))
    print(f"{output.name}  {output.stat().st_size:,} bytes")


if __name__ == "__main__":
    BUILD.mkdir(exist_ok=True)
    build("aarch64", "miniroot.tar.gz", "ld-musl-aarch64.so.1", emulator_drivers())
    build("armv7", "miniroot-armv7.tar.gz", "ld-musl-armhf.so.1", [])
