"""Cross-compile hostap's station state machine; only referenced sections enter Radio."""
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import subprocess


def android_target(common):
    """The NDK target whose libc headers these sources compile against, for the
    architecture `common` builds (AArch32 unless it says aarch64)."""
    if "--target=aarch64-none-elf" in map(str, common):
        return "--target=aarch64-linux-android21"
    return "--target=armv7a-linux-androideabi21"

FILES = """
rsn_supp/wpa rsn_supp/wpa_ie rsn_supp/pmksa_cache
common/wpa_common common/ieee802_11_common utils/common utils/wpabuf
crypto/aes-unwrap crypto/aes-wrap crypto/aes-internal crypto/aes-internal-dec crypto/aes-internal-enc
crypto/aes-omac1 crypto/md5 crypto/md5-internal crypto/sha1 crypto/sha1-internal
crypto/sha1-prf crypto/sha256 crypto/sha256-internal crypto/sha256-prf
""".split()

def objects(repo, out, common):
    src = repo / "build/sel4/wpa_supplicant-2.11/src"
    target = out / "wpa2"
    target.mkdir(exist_ok=True)
    flags = [*common, android_target(common), "-I", src,
             "-I", src / "utils", "-Dos_snprintf=station_format_disabled", "-include", Path(__file__).with_name("supplicant_port.h"), "-DCONFIG_NO_STDOUT_DEBUG", "-DCONFIG_NO_RANDOM_POOL",
             "-DCONFIG_CRYPTO_INTERNAL", "-DCONFIG_NO_RC4", "-DCONFIG_INTERNAL_AES", "-DCONFIG_INTERNAL_SHA1", "-DCONFIG_INTERNAL_SHA256",
             "-DCONFIG_INTERNAL_MD5", "-DCONFIG_NO_WPA_MSG", "-DCONFIG_NO_WPA_TRACE"]
    sources = [src / (name + ".c") for name in FILES]
    sources.append(Path(__file__).with_name("supplicant.c"))
    sources.append(Path(__file__).with_name("supplicant_os.c"))
    sources.append(Path(__file__).with_name("ap_wpa.c"))
    def compile(source):
        obj = target / (source.stem + ".o")
        if not obj.exists() or obj.stat().st_mtime_ns < max(
                source.stat().st_mtime_ns, Path(__file__).stat().st_mtime_ns):
            subprocess.run([*flags, "-c", source, "-o", obj], check=True)
        return obj
    with ThreadPoolExecutor(6) as pool:
        return list(pool.map(compile, sources))
