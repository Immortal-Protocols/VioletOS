"""Check both *finished images* before starting either guest, then boot both."""
import argparse
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from extract_spec import extract

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("image_dir", type=Path)
parser.add_argument("checker", type=Path)
args = parser.parse_args()
sdk = Path(os.environ["MICROKIT_SDK"])
fixtures = [("isolated", 0), ("granted", 2)]
digests = {}
with tempfile.TemporaryDirectory() as temp:
    for case, expected in fixtures:
        image = (args.image_dir / f"{case}.img").read_bytes()
        payload = Path(temp) / f"{case}.spec"
        payload.write_bytes(extract(image, sdk))
        digests[case] = hashlib.sha256(image).digest()
        result = subprocess.run([args.checker.resolve(), payload])
        if result.returncode != expected:
            raise SystemExit(f"{case}: checker returned {result.returncode}, expected {expected}")
    for case, _ in fixtures:
        image_path = args.image_dir / f"{case}.img"
        if hashlib.sha256(image_path.read_bytes()).digest() != digests[case]:
            raise SystemExit(f"{case}: image changed after static check")
        subprocess.run([sys.executable, Path(__file__).with_name("boot_test.py"), case, image_path], check=True)
