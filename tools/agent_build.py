#!/usr/bin/env python3
"""The Violet Agent's app workspace: where Codex writes and builds the apps a
user asks for on the phone (docs/agent-protocol.md, companion/mac).

    python3 tools/agent_build.py setup [DIR]   # create or refresh it, warm the build cache
    python3 tools/agent_build.py build NAME    # build DIR/apps/NAME for the phone

`setup` copies tools/agent-workspace (the recipe in AGENTS.md and the build
script), one working native example (Stopwatch, as apps/example) and the
crates the apps may use into DIR (default build/agent-workspace), then builds
the example once so later builds reuse the compiled violet crate. `build`
works offline and writes only inside DIR, so it runs inside Codex's
workspace-write sandbox. It prints a JSON line: the program's path, size and
build time.
"""
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import build_initfs  # noqa: E402

DEFAULT = ROOT / "build/agent-workspace"
TEMPLATE = ROOT / "tools/agent-workspace"


def environment(work):
    rustc, lld = build_initfs.toolchain()
    cargo = rustc.parent / "cargo"
    ndk = Path(os.environ.get("ANDROID_NDK_HOME", Path.home() / "Library/Android/sdk/ndk/28.2.13676358"))
    bins = list(ndk.glob("toolchains/llvm/prebuilt/*/bin"))
    if not bins:
        raise SystemExit("Install the Android NDK (its clang builds ring)")
    key = build_initfs.TARGET.replace("-", "_")
    env = dict(os.environ, **{
        f"CC_{key}": str(bins[0] / "clang"),
        f"AR_{key}": str(bins[0] / "llvm-ar"),
        f"CFLAGS_{key}": "--target=armv7-linux-musleabihf -march=armv7-a -mfpu=neon -mfloat-abi=hard",
        f"CARGO_TARGET_{key.upper()}_LINKER": str(lld),
        f"CARGO_TARGET_{key.upper()}_RUSTFLAGS": " ".join(["-C", "linker-flavor=ld.lld", *build_initfs.CPU_FLAGS]),
        "CARGO_TARGET_DIR": str(work / "target"),
        "CARGO_HOME": str(work / ".cargo-home"),
        "RUSTC": str(rustc),
        "TMPDIR": str(work / "target/tmp"),
    })
    (work / "target/tmp").mkdir(parents=True, exist_ok=True)
    return cargo, env


def setup(work):
    work.mkdir(parents=True, exist_ok=True)
    for f in TEMPLATE.iterdir():
        dest = work / f.name
        if f.is_file():
            shutil.copy2(f, dest)
    (work / "apps").mkdir(exist_ok=True)
    example = work / "apps/example"
    if example.exists():
        shutil.rmtree(example)
    shutil.copytree(ROOT / "tools/agent-example/stopwatch", example)
    toml = (example / "Cargo.toml").read_text().replace('name = "stopwatch"', 'name = "example"')
    (example / "Cargo.toml").write_text(toml)
    (example / "app.txt").write_text("Example\n")
    script = (work / "build").read_text().replace("@ROOT@", str(ROOT))
    (work / "build").write_text(script)
    (work / "build").chmod(0o755)
    violet = os.path.relpath(ROOT / "apps/lib/violet", work)
    cargo_toml = (work / "Cargo.toml").read_text().replace("@VIOLET@", violet)
    (work / "Cargo.toml").write_text(cargo_toml)
    # The crates violet needs, from the image build's registry, so builds run offline.
    home = work / ".cargo-home"
    if not home.exists():
        src = ROOT / "build/cargo-home"
        if src.exists():
            subprocess.run(["cp", "-c", "-R", str(src), str(home)], check=True)
        else:
            home.mkdir()
    cargo, env = environment(work)
    lock = work / "Cargo.lock"
    if not lock.exists() and (ROOT / "apps/Cargo.lock").exists():
        shutil.copy2(ROOT / "apps/Cargo.lock", lock)
    subprocess.run([cargo, "fetch"], cwd=work, env=env, check=True)
    return build(work, "example")


def build(work, name):
    app = work / "apps" / name
    if not (app / "Cargo.toml").exists():
        raise SystemExit(f"no app at {app} (make apps/{name}/Cargo.toml and src/main.rs)")
    cargo, env = environment(work)
    start = time.monotonic()
    r = subprocess.run([cargo, "build", "--offline", "--profile", "agent", "--target", build_initfs.TARGET, "-p", name],
                       cwd=work, env=env, capture_output=True, text=True)
    seconds = round(time.monotonic() - start, 1)
    if r.returncode:
        sys.stderr.write(r.stderr[-6000:])
        print(json.dumps({"ok": False, "name": name, "seconds": seconds}))
        return 1
    out = work / "target" / build_initfs.TARGET / "agent" / name
    title = (app / "app.txt").read_text().strip() if (app / "app.txt").exists() else name.title()
    print(json.dumps({"ok": True, "name": name, "title": title, "path": str(out), "bytes": out.stat().st_size, "seconds": seconds}))
    return 0


def main():
    args = sys.argv[1:]
    if not args or args[0] not in ("setup", "build"):
        raise SystemExit(__doc__)
    if args[0] == "setup":
        work = Path(args[1]).resolve() if len(args) > 1 else DEFAULT
        return setup(work)
    if len(args) < 2:
        raise SystemExit("usage: agent_build.py build NAME [DIR]")
    work = Path(args[2]).resolve() if len(args) > 2 else Path(os.environ.get("VIOLET_AGENT_WORKSPACE", Path.cwd())).resolve()
    return build(work, args[1])


if __name__ == "__main__":
    sys.exit(main())
