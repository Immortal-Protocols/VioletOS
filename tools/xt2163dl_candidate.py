"""Create the local XT2163DL boot candidate from exact-build inputs."""
import argparse
import hashlib
from pathlib import Path

from image import ROOT, archive, userspace
from phone_image import rebuild_v2

STOCK_SHA256 = "36adc5df15662da6140e14b81dbd2de4b0701164c2198f99608bee710d4dac0a"
# Paths within this release's vendor_a ext4 image.
TOUCH_FILES = {
    "lib/modules/mtk_tpd_mmi.ko": "734ed62a37b0744942d211a43a6cb062259a2c36bfe25640141d11eb37a4084c",
    "lib/modules/nt36xxx_mtk_mmi.ko": "2fa764fad9a704dbc0dcca55771fa560ec4a62c40e1e271c30f7ae4ab79f54b0",
    "firmware/djn_nt36525b_a_novatek_ts_fw.bin": "7269992be538e72af63699947b545511cfe7cff482edabbd662c7e45cfe7e544",
    "firmware/djn_nt36525b_novatek_ts_fw.bin": "5ee88aa50aa4d0262fb826f37dabef662233d943e9321c9595d0189de6ef07c3",
    "firmware/tm_nt36672a_novatek_ts_fw.bin": "101efa101aa2f949f9a261043acf068eee488f43c5176e1eecb649081bfadd8e",
}


def checked(path, expected):
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f"Unexpected input digest: {path}")
    return data


def candidate(stock_path, vendor_dir, *, kernel=None):
    stock = checked(stock_path, STOCK_SHA256)
    entries = userspace("armv7", "miniroot-armv7.tar.gz", "ld-musl-armhf.so.1",
                        ROOT / "guest/init-xt2163dl")
    entries.append(("bin/touch-log", 0o100755, (ROOT / "guest/touch-log").read_bytes()))
    for directory in ("vendor", "vendor/lib", "vendor/lib/modules", "vendor/firmware"):
        entries.append((directory, 0o40755, b""))
    # Support both the stock kernel command-line path and the standard Linux lookup.
    entries.append(("lib/firmware", 0o120777, b"/vendor/firmware"))
    for name, digest in TOUCH_FILES.items():
        entries.append(("vendor/" + name, 0o100644, checked(vendor_dir / name, digest)))
    data = rebuild_v2(stock, archive(entries), kernel=kernel)
    if len(data) > len(stock):
        raise ValueError("Candidate exceeds the handset boot partition size")
    return data


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stock", type=Path)
    parser.add_argument("vendor_files", type=Path, help="directory containing the five pinned vendor files")
    parser.add_argument("output", type=Path, help="new local image path")
    parser.add_argument("--kernel", type=Path, help="use a locally built ARM zImage")
    args = parser.parse_args()
    data = candidate(args.stock, args.vendor_files, kernel=args.kernel.read_bytes() if args.kernel else None)
    with args.output.open("xb") as output:
        output.write(data)
    print(f"{len(data):,} bytes  sha256={hashlib.sha256(data).hexdigest()}")
