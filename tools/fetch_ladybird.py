"""Fetch the parts of Ladybird that the VioletOS browser builds on.

The browser in apps/open/ladybird links Ladybird's HTML tokenizer and tree
builder (Libraries/LibWeb/HTML/Parser/Rust) and the AK crate under it. This
script checks out exactly those paths of one pinned Ladybird commit into
build/ladybird with a sparse, blob-less clone, so the download stays small.
Run it on its own, or let tools/build_initfs.py call it.
"""
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "https://github.com/LadybirdBrowser/ladybird.git"
COMMIT = "c87319cd8b89404aff26d791fb6a2c8007fe1b65"
DESTINATION = ROOT / "build/ladybird"
PATHS = [
    "/LICENSE",
    "/rust-toolchain.toml",
    "/Cargo.toml",
    "/Cargo.lock",
    "/AK/Rust/",
    "/Libraries/LibWeb/HTML/Parser/Rust/",
    "/Libraries/LibWeb/HTML/Parser/Entities.json",
    "/Libraries/LibWeb/HTML/TagNames.h",
    "/Libraries/LibWeb/HTML/AttributeNames.h",
    "/Libraries/LibWeb/SVG/TagNames.h",
    "/Libraries/LibWeb/MathML/TagNames.h",
]


def git(*args):
    subprocess.run(["git", "-C", str(DESTINATION), *args], check=True, stdout=subprocess.DEVNULL)


def current():
    result = subprocess.run(["git", "-C", str(DESTINATION), "rev-parse", "HEAD"], capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else None


def trim_workspace():
    """Keep only the workspace members this checkout has. Cargo (and cbindgen,
    through cargo metadata) loads every member of Ladybird's workspace."""
    manifest = DESTINATION / "Cargo.toml"
    lines = manifest.read_text().splitlines(keepends=True)
    kept = []
    in_members = False
    for line in lines:
        stripped = line.strip()
        if stripped.startswith("members = ["):
            in_members = True
        elif in_members and stripped == "]":
            in_members = False
        elif in_members and stripped.startswith('"'):
            if not (DESTINATION / stripped.strip('",') / "Cargo.toml").exists():
                continue
        kept.append(line)
    manifest.write_text("".join(kept))


def main():
    if current() == COMMIT and all((DESTINATION / p.strip("/")).exists() for p in PATHS):
        trim_workspace()
        return DESTINATION
    DESTINATION.mkdir(parents=True, exist_ok=True)
    if not (DESTINATION / ".git").exists():
        git("init", "-q")
        git("remote", "add", "origin", REPOSITORY)
    git("config", "core.sparseCheckout", "true")
    (DESTINATION / ".git/info/sparse-checkout").write_text("\n".join(PATHS) + "\n")
    git("fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", COMMIT)
    git("checkout", "-q", "--force", COMMIT)
    if current() != COMMIT:
        raise SystemExit(f"Ladybird checkout is not at {COMMIT}")
    trim_workspace()
    return DESTINATION


if __name__ == "__main__":
    print(main())
    sys.exit(0)
