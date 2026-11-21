"""Build the pinned TLS library with the guest compiler; no host TLS linkage."""

from concurrent.futures import ThreadPoolExecutor
import hashlib
from pathlib import Path
import subprocess


def build(repo, out, flags):
    src = repo / "build/sel4/bearssl-0.6"
    local = repo / "services/radio"
    args = flags + [
        "-DBR_USE_UNIX_TIME=0",
        "-DBR_USE_URANDOM=0",
        "-DBR_USE_GETENTROPY=0",
        "-I",
        src / "inc",
        "-I",
        src / "src",
        "-I",
        local / "freestanding",
    ]
    key = hashlib.sha256(
        (str(args) + str((local / "freestanding/string.h").read_bytes())).encode()
    ).hexdigest()[:12]
    obj = out / ("bearssl-" + key)
    obj.mkdir(exist_ok=True)
    archive = obj / "libbearssl.a"
    if not archive.exists():

        def compile(file):
            dest = obj / (file.stem + ".o")
            subprocess.run([*args, "-c", file, "-o", dest], check=True)
            return dest

        with ThreadPoolExecutor(8) as pool:
            objects = list(pool.map(compile, sorted((src / "src").rglob("*.c"))))
        tmp = obj / "new.a"
        subprocess.run(
            [flags[0].with_name("llvm-ar"), "rcs", tmp, *objects], check=True
        )
        tmp.replace(archive)
    shim = obj / "client.o"
    subprocess.run([*args, "-c", local / "tls.c", "-o", shim], check=True)
    return [shim, archive]
