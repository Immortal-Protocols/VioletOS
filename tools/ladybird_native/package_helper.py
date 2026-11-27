"""Archive upstream Ladybird helpers, their libraries and browser resources.

Run on the ARM64 build machine with the upstream package installed. The files
retain their guest paths so PT_INTERP, RUNPATH and the system library search
directories work on VioletOS. The Hub also needs system fonts to start its
WebContent helpers.
"""
import argparse
from io import BytesIO
import os
from pathlib import Path
import subprocess
import sys
import tarfile


def link_files(executable):
    files = {executable}
    result = subprocess.check_output(["ldd", str(executable)], text=True)
    for line in result.splitlines():
        if "=> not found" in line:
            raise SystemExit(line.strip())
        path = line.split(" => ")[-1].strip().split(" (0x")[0]
        if path.startswith("/"):
            files.add(Path(os.path.normpath(path)))
    return sorted(files)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("helpers", nargs="+", choices=["WebContent", "RequestServer", "MediaServer", "VideoDecoderServer",
                                          "AudioServer", "ImageDecoder", "Compositor", "SystemProbe", "WebRTCClient", "Hub"])
    parser.add_argument("--document", type=Path, help="bundle a local page at /document.html")
    args = parser.parse_args()
    files = set()
    for helper in args.helpers:
        files.update(link_files(Path("/opt/ladybird/libexec") / helper))
    files.update(p for p in Path("/opt/ladybird/share/Lagom").rglob("*") if p.is_file())
    fonts = []
    if "Hub" in args.helpers:
        font_dir = Path("/usr/share/fonts")
        for name in ("DejaVuSans", "DejaVuSansMono", "DejaVuSerif", "LiberationSans-Regular", "LiberationSans-Bold",
                     "LiberationMono-Regular", "LiberationMono-Bold", "LiberationSerif-Regular", "LiberationSerif-Bold"):
            candidates = sorted(font_dir.rglob(f"{name}.ttf"))
            if candidates:
                fonts.append(candidates[0])
        if not any("Mono" in f.stem for f in fonts):
            parser.error("install DejaVu or Liberation fonts on the packaging machine before archiving Hub")
    with tarfile.open(fileobj=sys.stdout.buffer, mode="w|gz", dereference=True) as archive:
        for path in sorted(files):
            archive.add(path, arcname=str(path.relative_to("/")), recursive=False)
        for font in fonts:
            archive.add(font, arcname=f"usr/share/fonts/{font.name}", recursive=False)
        metadata = {}
        if "Hub" in args.helpers or "RequestServer" in args.helpers:
            sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
            from build_initfs import network_files
            metadata.update(network_files())
            if not metadata["etc/ssl/certs/ca-certificates.crt"]:
                parser.error("install ca-certificates on the packaging machine")
        if fonts:
            config = (Path(__file__).parent / "fonts.conf").read_bytes()
            metadata["etc/fonts/fonts.conf"] = config
        for name, data in metadata.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            archive.addfile(info, BytesIO(data))
        if args.document:
            archive.add(args.document, arcname="document.html", recursive=False)


if __name__ == "__main__":
    main()
