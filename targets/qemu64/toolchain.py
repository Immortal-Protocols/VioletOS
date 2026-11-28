"""The Rust standard libraries the AArch64 build needs, in toolchain copies under this tree's build/.

    python3 targets/qemu64/toolchain.py --check      say which are missing; downloads nothing
    python3 targets/qemu64/toolchain.py              add the missing ones
    python3 targets/qemu64/toolchain.py --ladybird   also give Ladybird's Rust the AArch64 musl target

The components (supervisor libraries, services, Open Runtime) need
aarch64-unknown-none; the open apps need aarch64-unknown-linux-musl. The
toolchain is the pinned 1.91.1 the AArch32 build uses. It is never changed
where it is (it may be another checkout's): it is copied (an APFS clone, so
it costs no space) to build/rust-aarch64/rustup, and rustup adds the official
rust-std components from static.rust-lang.org there, checked against the
release's signed channel manifest as rustup always does. Ladybird's toolchain
(build/rust198, VIOLET_LADYBIRD_RUSTC) is already this tree's own, so its
target is added in place. tools/build_initfs.py and targets/qemu64/stack.py
find both through build/*/rustup/toolchains/*/bin/rustc.
"""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys

REPO = Path(__file__).resolve().parents[2]
VERSION = "1.91.1"
HOST = "aarch64-apple-darwin"
TOOLCHAIN = f"{VERSION}-{HOST}"
TARGETS = ["aarch64-unknown-none", "aarch64-unknown-linux-musl"]
HOME = REPO / "build/rust-aarch64/rustup"
LADYBIRD_HOME = REPO / "build/rust198/rustup"


def checkouts():
    """This tree, then the main checkout it is a worktree of (whose build/ holds the shared toolchains)."""
    found = [REPO]
    common = subprocess.run(["git", "rev-parse", "--git-common-dir"], cwd=REPO, capture_output=True, text=True).stdout.strip()
    if common:
        main = (REPO / common).resolve().parent
        if main != REPO:
            found.append(main)
    return found


def source_toolchain():
    for root in checkouts():
        candidate = root / "build/spike1/rustup/toolchains" / TOOLCHAIN
        if (candidate / "bin/rustc").exists():
            return candidate
    return None


def rustup():
    for candidate in [shutil.which("rustup"), *(str(r / "build/spike1/cargo/bin/rustup") for r in checkouts())]:
        if candidate and Path(candidate).exists():
            return candidate
    sys.exit("No rustup found: install it, or set PATH to one")


def missing(toolchain, targets):
    return [t for t in targets if not (toolchain / "lib/rustlib" / t).is_dir()]


def ladybird_toolchain():
    found = sorted(LADYBIRD_HOME.glob("toolchains/*/bin/rustc"))
    return found[-1].parents[1] if found else None


def add(home, toolchain, targets):
    env = dict(os.environ, RUSTUP_HOME=str(home), RUSTUP_TOOLCHAIN=toolchain.name)
    subprocess.run([rustup(), "target", "add", "--toolchain", toolchain.name, *targets], env=env, check=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="report only")
    parser.add_argument("--ladybird", action="store_true", help="also Ladybird's toolchain (build/rust198)")
    args = parser.parse_args()
    copy = HOME / "toolchains" / TOOLCHAIN
    plan = []
    if (copy / "bin/rustc").exists():
        plan.append((HOME, copy, missing(copy, TARGETS)))
    else:
        source = source_toolchain()
        if source is None:
            sys.exit(f"No Rust {TOOLCHAIN} under build/spike1 here or in the main checkout")
        plan.append((HOME, copy, missing(source, TARGETS) if args.check else None, source))
    if args.ladybird:
        ladybird = ladybird_toolchain()
        if ladybird is None:
            sys.exit(f"No toolchain under {LADYBIRD_HOME}")
        plan.append((LADYBIRD_HOME, ladybird, missing(ladybird, TARGETS[1:])))
    problems = 0
    for home, toolchain, need, *source in plan:
        if source and not args.check:
            print(f"copying {source[0]} to {toolchain}")
            toolchain.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(["cp", "-Rc", source[0], toolchain], check=True)
            (home / "settings.toml").write_text(f'version = "12"\ndefault_toolchain = "{TOOLCHAIN}"\n')
            need = missing(toolchain, TARGETS)
        where = f"{toolchain}{' (to be copied from ' + str(source[0]) + ')' if source and args.check else ''}"
        if not need:
            print(f"{where}: has {', '.join(TARGETS if home == HOME else TARGETS[1:])}")
            continue
        if args.check:
            print(f"{where}: missing {', '.join(need)}")
            problems += 1
            continue
        add(home, toolchain, need)
        left = missing(toolchain, need)
        if left:
            sys.exit(f"{toolchain}: rustup did not install {', '.join(left)}")
        print(f"{toolchain}: added {', '.join(need)}")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
