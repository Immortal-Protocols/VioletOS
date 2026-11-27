"""Prepare or build the exact upstream Ladybird release with VioletOS terminal audio."""
import argparse
import hashlib
from pathlib import Path
import shutil
import subprocess
import tempfile

from build_runtime import HERE, download

RELEASE = "2026.47.8"
COMMIT = "0be55a57cc07ed258362c631a13a10bb4f6d707a"
ARCHIVE = {
    "name": f"ladybird-{COMMIT}.tar.gz",
    "url": f"https://codeload.github.com/LadybirdBrowser/ladybird/tar.gz/{COMMIT}",
    "sha256": "ffc28a92c2c9410939770c0a4ec3ec59bacb9382bd0e6b68567b467adf94131e",
}


def prepare(work):
    work.mkdir(parents=True, exist_ok=True)
    target = work / "source"
    patch = HERE / "release-audio.patch"
    backend = HERE / "PlaybackStreamViolet.cpp"
    stamp = hashlib.sha256(ARCHIVE["sha256"].encode() + patch.read_bytes() + backend.read_bytes()).hexdigest()
    if target.exists():
        installed = target / ".violet-audio-source"
        if installed.is_file() and installed.read_text() == stamp:
            return target
        raise SystemExit(f"The source or backend changed. Choose a fresh --out directory (existing {target}).")
    source_archive = download(ARCHIVE, work)
    with tempfile.TemporaryDirectory(dir=work) as tmp:
        source = Path(tmp) / "source"
        source.mkdir()
        subprocess.run(["tar", "-xf", source_archive, "-C", source, "--strip-components", "1"], check=True)
        subprocess.run(["patch", "-s", "-p1", "-i", patch], cwd=source, check=True)
        shutil.copy2(backend, source / "Services/AudioServer/PlaybackStreamViolet.cpp")
        (source / "REVISION").write_text(COMMIT + "\n")
        (source / ".violet-audio-source").write_text(stamp)
        source.rename(target)
    return target


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=HERE.parents[1] / "build/ladybird-native/release-audio")
    parser.add_argument("--build", action="store_true", help="configure upstream's Release preset and build Hub and AudioServer")
    args = parser.parse_args()
    work = args.out.resolve()
    source = prepare(work)
    print(f"Upstream Ladybird {RELEASE} ({COMMIT}) prepared at {source}", flush=True)
    if args.build:
        build = work / "build"
        subprocess.run(["cmake", "--preset", "Release", "-B", build, "-DLADYBIRD_VIOLET_AUDIO=ON"],
                       cwd=source, check=True)
        subprocess.run(["cmake", "--build", build, "--target", "AudioServer", "Hub"], check=True)
