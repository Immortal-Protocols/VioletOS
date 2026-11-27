"""Build the native seL4 Home and Ladybird desktop with network, input and sound."""
import argparse
import fcntl
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import build_initfs
from build_python import ndk_bin


def sdk_directories():
    repo = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "--git-common-dir"],
                          capture_output=True, text=True)
    caches = [ROOT / "build/spike1"]
    if repo.returncode == 0:
        common = (ROOT / repo.stdout.strip()).resolve()
        caches.append(common.parent / "build/spike1")
    paths = {}
    for key, name in [("MICROKIT_SDK","microkit-sdk-2.4.0"),
                      ("MICROKIT_SRC","microkit-2.4.0"), ("SEL4_SRC","seL4-17.0.0")]:
        candidate = os.environ.get(key)
        path = Path(candidate) if candidate else next((c/name for c in caches if (c/name).is_dir()), None)
        if path is None or not path.is_dir():
            raise SystemExit(f"Set {key} to the {name} directory (see targets/qemu/aarch64/README.md)")
        paths[key] = str(path.resolve())
    return paths


def install_engine(source, guest, required):
    # Pack regular guest files, including aliases of files inside the self-contained package.
    source = source.resolve(strict=True)
    for name in required:
        if not (source / name).is_file():
            raise SystemExit(f"{source} is missing {name}")
    for file in sorted(source.rglob("*")):
        resolved = file.resolve(strict=True)
        if not resolved.is_relative_to(source):
            raise SystemExit(f"Engine package link leaves its root: {file}")
        if file.is_file():
            name = file.relative_to(source)
            dest = guest / name
            # Standalone browser packages include their build machine's CA
            # store. An installed desktop already supplies VioletOS's CA store.
            if name.as_posix() == "etc/ssl/certs/ca-certificates.crt" and dest.exists():
                if dest.stat().st_size:
                    continue
                shutil.copy2(resolved, dest)
                continue
            if dest.exists() and dest.read_bytes() != resolved.read_bytes():
                raise SystemExit(f"Engine package conflicts with the installed file /{name}")
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(resolved, dest)
    print(f"Including the native browser package from {source}", flush=True)


def install_release_services(source, guest):
    sys.path.insert(0, str(ROOT / "tools/ladybird_native"))
    from prepare_release_audio import COMMIT
    path = Path("opt/ladybird/libexec")
    replacement = source / path
    for hub in (guest / path / "Hub", replacement / "Hub"):
        if COMMIT.encode() not in hub.read_bytes():
            raise SystemExit(f"The release audio backend requires Hub from commit {COMMIT}: {hub}")
    for name in ("Hub", "AudioServer"):
        shutil.copy2(replacement / name, guest / path / name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=ROOT / "build/sel4/qemu-aarch64-desktop")
    packages = parser.add_mutually_exclusive_group()
    packages.add_argument("--browser-root", type=Path, default=os.environ.get("LADYBIRD_BROWSER_ROOT"),
                          help="complete upstream AArch64 browser package with its matching helper processes")
    packages.add_argument("--engine-root", type=Path, default=os.environ.get("LADYBIRD_ENGINE_ROOT"),
                        help="guest files for /usr/libexec/ladybird-dom, including fonts and ICU data")
    parser.add_argument("--release-services", type=Path, default=os.environ.get("LADYBIRD_RELEASE_SERVICES"),
                        help="VioletOS Hub and AudioServer guest overlay built by build_release_services.py")
    tests = parser.add_mutually_exclusive_group()
    tests.add_argument("--youtube-iframe-test", action="store_true", help="open Google's public YouTube IFrame API sample")
    tests.add_argument("--open", metavar="URL", help="boot the networked desktop directly into Ladybird at this URL")
    tests.add_argument("--upstream-media-test", action="store_true", help="boot the upstream MediaSource click-to-play fixture offline")
    tests.add_argument("--presentation-test", action="store_true", help="boot the local interactive LibWeb test page")
    args = parser.parse_args()
    if args.youtube_iframe_test:
        from youtube_test import IFRAME_SAMPLE
        args.open = IFRAME_SAMPLE
    if (args.presentation_test or args.upstream_media_test or args.youtube_iframe_test) and not args.browser_root:
        parser.error("the upstream browser test needs --browser-root or LADYBIRD_BROWSER_ROOT")
    if args.release_services and not args.browser_root:
        parser.error("--release-services requires --browser-root or LADYBIRD_BROWSER_ROOT")
    engine = args.engine_root
    pinned = ROOT / "build/ladybird-native/document/root"
    if engine and args.browser_root:
        parser.error("select one browser package")
    if not args.browser_root and engine is None and (pinned / "usr/libexec/ladybird-dom").is_file():
        engine = pinned
    env = dict(os.environ, **sdk_directories())
    # Kernel code generators and the loader build need the seL4 Python modules and LLVM tools.
    env["PATH"] = os.pathsep.join([str(ROOT / "build/sel4/venv/bin"), str(ndk_bin()), env["PATH"]])
    env.setdefault("CMAKE_POLICY_VERSION_MINIMUM", "3.5")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    with (out / "build.lock").open("a+") as lock, tempfile.TemporaryDirectory(dir=out) as tmp:
        fcntl.flock(lock, fcntl.LOCK_EX)
        guest = Path(tmp) / "root"
        build_initfs.main(guest, browser_aarch64=True, directory=True)
        if args.browser_root:
            helpers = ("Hub", "WebContent", "RequestServer", "Compositor", "MediaServer", "VideoDecoderServer", "AudioServer", "ImageDecoder", "SystemProbe")
            install_engine(args.browser_root, guest, [f"opt/ladybird/libexec/{name}" for name in helpers])
            if args.release_services:
                install_release_services(args.release_services, guest)
        elif engine is not None:
            install_engine(engine, guest, ["usr/libexec/ladybird-dom"])
        if args.upstream_media_test:
            from upstream_media import write_fixture
            write_fixture(guest / "upstream-media.html")
            boot = ["--elf", guest / "apps/ladybird", "--arg=--trace-upstream", "--arg=file:///upstream-media.html"]
        elif args.presentation_test:
            shutil.copy2(HERE / "presentation.html", guest / "presentation.html")
            boot = ["--elf", guest / "apps/ladybird", "--arg=--trace-upstream", "--arg=file:///presentation.html"]
        elif args.open:
            boot = ["--elf", guest / "apps/ladybird", "--network", "--arg=--trace-upstream", "--arg=" + args.open]
        else:
            boot = ["--elf", guest / "apps/home", "--network"]
        subprocess.run([sys.executable, HERE / "build.py", "--out", out, "--root", guest, *boot], env=env, check=True)
    print(f"Native AArch64 desktop image: {out / 'violet.img'}")


if __name__ == "__main__":
    main()
