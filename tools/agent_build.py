#!/usr/bin/env python3
"""The Violet Agent's app workspace: where Codex writes and builds the apps a
user asks for on the phone (docs/agent-protocol.md, companion/mac).

    python3 tools/agent_build.py setup [DIR]   # create or refresh it, warm the build cache
    python3 tools/agent_build.py build NAME    # build DIR/apps/NAME for the phone

`setup` copies tools/agent-workspace (the recipe in AGENTS.md and the build
script), one working native example (Stopwatch, as apps/example), a Python
example (apps/pyexample) with the violet Python module, and the crates the
apps may use into DIR (default build/agent-workspace), then builds the native
example once so later builds reuse the compiled violet crate. `build` works
offline and writes only inside DIR, so it runs inside Codex's workspace-write
sandbox. It prints a JSON line: the program's path, size and build time.

An app's manifest.txt (next to its app.txt) says what it may use on the
phone ("caps: net mic camera audio-out hci"; runtime/open/policy.rs). The
program at `path` then ends with it (the manifest, its length, "VMF1"),
which the phone splits off at install. Without one the app gets only its
own /data, the screen and touch. Net, the microphone and the camera are
asked of the user the first time the app uses them.

A folder python/NAME with main.py is a Python app: `build` checks it with this Mac's
Python 3.12, runs it once against a fake screen with taps across it (errors
fail the build), and writes it as one #!/bin/python3 file.
"""
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import build_initfs  # noqa: E402
import elf_check  # noqa: E402

DEFAULT = ROOT / "build/agent-workspace"
TEMPLATE = ROOT / "tools/agent-workspace"
HOST_PYTHON = os.environ.get("VIOLET_HOST_PYTHON", "/opt/homebrew/bin/python3.12")
# Taps the check run makes: down the middle and across the usual button rows.
CHECK_TAPS = ";".join(f"{x},{y}" for y in (300, 500, 700, 900, 1100, 1300, 1500) for x in (180, 540))
CHECK_SECONDS = 8


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
        f"CARGO_TARGET_{key.upper()}_RUSTFLAGS": " ".join(["-C", "linker-flavor=ld.lld", *build_initfs.CPU_FLAGS,
                                                               *map(str, build_initfs.pie_flags(rustc))]),
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
    # Python: the example app and the violet module (with its fonts) for check runs.
    pyexample = work / "python/tally"
    if pyexample.exists():
        shutil.rmtree(pyexample)
    shutil.rmtree(work / "apps/pyexample", ignore_errors=True)
    pyexample.parent.mkdir(exist_ok=True)
    shutil.copytree(TEMPLATE / "pyexample", pyexample)
    pylib = work / "pylib"
    pylib.mkdir(exist_ok=True)
    shutil.copy2(ROOT / "apps/lib/python/violet.py", pylib / "violet.py")
    shutil.copy2(ROOT / "apps/lib/python/violet.py", work / "violet.py")
    font = ROOT / "build/violet-font.bin"
    if font.exists():
        shutil.copy2(font, pylib / "violet_font.bin")
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


def build_python(work, name):
    """Check a Python app on this Mac, then write it as one file for /apps."""
    app = work / "python" / name
    source = (app / "main.py").read_text()
    start = time.monotonic()
    out_dir = work / "target/python"
    out_dir.mkdir(parents=True, exist_ok=True)
    title = (app / "app.txt").read_text().strip() if (app / "app.txt").exists() else name.title()

    def fail(text):
        sys.stderr.write(text[-6000:])
        print(json.dumps({"ok": False, "name": name, "seconds": round(time.monotonic() - start, 1)}))
        return 1

    if not Path(HOST_PYTHON).exists():
        return fail(f"Python 3.12 is not installed on this Mac ({HOST_PYTHON})\n")
    r = subprocess.run([HOST_PYTHON, "-c", "import ast,sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])", str(app / "main.py")],
                       capture_output=True, text=True)
    if r.returncode:
        return fail(r.stderr)
    data = out_dir / f"check-data-{name}"
    shutil.rmtree(data, ignore_errors=True)
    data.mkdir()
    env = dict(os.environ, VIOLET_FAKE="1", VIOLET_FAKE_EVENTS=CHECK_TAPS, VIOLET_FAKE_DATA=str(data),
               VIOLET_FAKE_SCREEN=str(out_dir / f"{name}.ppm"), PYTHONPATH=str(work / "pylib"),
               PYTHONDONTWRITEBYTECODE="1", TMPDIR=str(work / "target/tmp"))
    (work / "target/tmp").mkdir(parents=True, exist_ok=True)
    try:
        r = subprocess.run([HOST_PYTHON, str(app / "main.py")], cwd=data, env=env, capture_output=True, text=True,
                           timeout=CHECK_SECONDS)
        if r.returncode:
            return fail("The app failed its check run (a fake screen, taps across it):\n" + r.stderr)
        checked = "ran to the end of its check taps"
    except subprocess.TimeoutExpired:
        checked = f"still running after {CHECK_SECONDS} s of check taps, without errors"
    out = out_dir / name
    if not source.startswith("#!"):
        source = "#!/bin/python3\n" + source
    try:
        out.write_bytes(with_manifest(app, source.encode()))
    except SystemExit as e:
        return fail(f"{e}\n")
    seconds = round(time.monotonic() - start, 1)
    print(json.dumps({"ok": True, "name": name, "title": title, "path": str(out), "bytes": out.stat().st_size,
                      "seconds": seconds, "language": "python", "check": checked}))
    return 0


def with_manifest(app, program):
    """The program as the phone installs it: with the app's manifest at the end, if it has one."""
    manifest = app / build_initfs.MANIFEST
    if not manifest.exists():
        return program
    return build_initfs.envelope(manifest.read_text(), program)


def build(work, name):
    # runtime/open/appstore.rs NEW_NAME: /data/NAME.KEY must fit Storage's 16 characters.
    if not re.fullmatch(r"[a-z0-9_-]{1,12}", name):
        raise SystemExit(f"app name {name!r}: up to 12 of a-z 0-9 _ - (the phone keeps /data/NAME.KEY in 16 characters)")
    python_app = work / "python" / name
    is_python = (python_app / "main.py").is_file()
    app = python_app if is_python else work / "apps" / name
    if not (app / build_initfs.MANIFEST).exists():
        for filename in ("manifest.yaml", "manifest.yml"):
            candidate = app / filename
            if candidate.is_file() and re.search(r"^\s*caps\s*:", candidate.read_text(), re.MULTILINE):
                raise SystemExit(f"Put the app capabilities in {app / build_initfs.MANIFEST} before building; {filename} is ignored.")
    if is_python:
        return build_python(work, name)
    if not (app / "Cargo.toml").exists():
        raise SystemExit(f"no app at {app} (make python/{name}/main.py, or apps/{name}/Cargo.toml and src/main.rs)")
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
    # The same gate as the system image: a position-independent program the
    # phone's loader takes (W^X, code in its own pages, relocations it can apply).
    try:
        elf_check.check({name: out.read_bytes()}, allow_fixed=elf_check.fixed_allowed())
    except SystemExit as e:
        sys.stderr.write(f"{e}\n")
        print(json.dumps({"ok": False, "name": name, "seconds": seconds}))
        return 1
    if (app / build_initfs.MANIFEST).exists():
        # Cargo's own output stays as it is; the phone gets the program with its manifest.
        packed = work / "target/agent-out" / name
        packed.parent.mkdir(parents=True, exist_ok=True)
        try:
            packed.write_bytes(with_manifest(app, out.read_bytes()))
        except SystemExit as e:
            sys.stderr.write(f"{e}\n")
            print(json.dumps({"ok": False, "name": name, "seconds": seconds}))
            return 1
        out = packed
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
