#!/usr/bin/env python3
"""Bring Rose (the RoseOS voice agent) into companion/mac/roseos, unchanged.

    python3 tools/sync_rose.py [--rose ~/RoseOS-1] [--rev origin/main]

Violet Agent for macOS runs Rose's own agent: her listener and speaker, the
Codex conductor, tasks, identity and voices, transcripts, and her life and
journal. This copies those files from a RoseOS checkout at a commit, keeping
RoseOS's folder layout (Rose finds her Codex bridge and assets by paths
relative to it), and records the commit in companion/mac/roseos/ROSE_VERSION.
Violet's own parts live beside it in companion/mac/violet_rose and never
edit these files, so a newer Rose is one run of this script.

Left out: Android-only inputs of Rose's app builder (the Gradle starter
templates and the Java sign-in source); VioletOS builds its apps itself.
"""
import argparse
import io
import re
import shutil
import subprocess
import tarfile
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / "companion/mac/roseos"
DEMO = "prototypes/voice-phone-demo"
# Rose herself, her setup, and the files she reaches through the RoseOS layout.
PATHS = [
    f"{DEMO}/rose",
    f"{DEMO}/requirements.txt",
    f"{DEMO}/scripts",
    f"{DEMO}/README.md",
    f"{DEMO}/REMOTE.md",
    "workspace/rose-agent/app/assets/customization-agent-tools.json",
    "workspace/rose-agent/server/test-fixtures",
    "workspace/models/rose_cloud",
    "docs/MY-DUDE.md",
    "docs/BACKGROUND-LIFE.md",
    "docs/CODEX-VOICE.md",
    "docs/COMPUTER-LINK.md",
    "docs/APP-BUILDER.md",
    "docs/APP-BUILD.md",
]
BRIDGE = "workspace/rose-agent/server/codex-voice-bridge.mjs"


def git(rose, *args, text=True):
    return subprocess.run(["git", "-C", str(rose), *args], capture_output=True, text=text, check=True).stdout


def bridge_files(rose, rev):
    """The Codex voice bridge and every module it imports, relatively."""
    todo, seen = [BRIDGE], set()
    while todo:
        p = todo.pop()
        if p in seen:
            continue
        seen.add(p)
        src = git(rose, "show", f"{rev}:{p}")
        for m in re.findall(r"""(?:from\s+|import\s*\(\s*|import\s+)['"](\.[^'"]+)['"]""", src):
            todo.append("/".join(_normalize(f"{PurePosixPath(p).parent}/{m}")))
    return sorted(seen)


def _normalize(path):
    parts = []
    for part in path.split("/"):
        if part == "..":
            parts.pop()
        elif part not in ("", "."):
            parts.append(part)
    return parts


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--rose", type=Path, default=Path.home() / "RoseOS-1")
    ap.add_argument("--rev", default="origin/main")
    args = ap.parse_args()
    rev = git(args.rose, "rev-parse", args.rev).strip()
    files = PATHS + bridge_files(args.rose, rev)
    archive = git(args.rose, "archive", "--format=tar", rev, *files, text=False)
    if DEST.exists():
        shutil.rmtree(DEST)
    DEST.mkdir(parents=True)
    with tarfile.open(fileobj=io.BytesIO(archive)) as t:
        # A git archive of our own checkout: only regular files and folders.
        t.extractall(DEST)
    subject = git(args.rose, "log", "-1", "--format=%cs %s", rev).strip()
    (DEST / "ROSE_VERSION").write_text(f"{rev}\n{subject}\n")
    count = sum(1 for p in DEST.rglob("*") if p.is_file())
    print(f"Rose {rev[:8]} ({subject}): {count} files -> {DEST.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
