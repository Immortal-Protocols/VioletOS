"""Pack the exact XT2163DL vendor radio inputs into a read-only boot resource."""
from hashlib import sha256
from pathlib import Path
from struct import pack

FILES = (
    ("soc1_0_ram_mcu_1_1_hdr.bin", "731fb22e76b7a758fc8525203ff6d7f17b5ec051cc48562cf552235a3e9e1300"),
    ("soc1_0_ram_wifi_1_1_hdr.bin", "3179999c5262e97b9ee720a4b6e6dfc1400e27f768e5d45805e9ce1a3f6104e3"),
    ("soc1_0_patch_mcu_1_1_hdr.bin", "c5c0a9d68786e779abb0cbf4d3ea2193903dd6214b77f781ee86a3438fd5fab1"),
    ("WIFI_RAM_CODE_soc1_0_1_1.bin", "5f6c59641b223b807837e27fa1e17f03380791df0f1b212343961e59127c780e"),
    ("WMT_SOC_ELLIS.cfg", "6845e0f30219d5aedaf3791b8318eaaf7ae22d0087702268a815c8724243e2e8"),
    ("WMT_SOC_ELLIS_EPA.cfg", "c429c3af3209bd9bc5065b59830a567d300da4ea26d67124c65f129c51a940ff"),
    ("soc1_0_ram_bt_1_1_hdr.bin", "5e720ce940deaf581ceeb5f2aba4e8834ed13d62177fb4e21f7b4edcd74337fc"),
    ("wifi.cfg", "2528589ddbfdc59e7e3e9dfcd3a29736ca923b7978f503d8bf28bb4017cf080e"),
    ("wifi_EPA.cfg", "ee794b0b09897907ff947bbc4c86a647b82dd859070fa0819590912f2a979f5c"),
    ("txpowerctrl.cfg", "7d7f8a72791baf3addeda33be4e677cf291c1b03731e7b5868c72d543f20e10b"),
    ("txpowerctrl_EPA.cfg", "c94c12e9ddd0b6d8076832822652e66950ac56077a51e84439ce565f007bfac2"),
)


def prepare_wifi(stock: Path, output: Path):
    header = bytearray(b"VRF2" + pack("<I", len(FILES)))
    payload = bytearray()
    for name, digest in FILES:
        path = stock / "wifi" / name
        if not path.is_file():
            raise ValueError(f"Copy vendor/firmware/{name} from the XT2163DL factory image to {path}")
        data = path.read_bytes()
        if sha256(data).hexdigest() != digest:
            raise ValueError(f"Unexpected XT2163DL firmware digest for {name}")
        header.extend(pack("<II", 8 + len(FILES) * 8 + len(payload), len(data)))
        payload.extend(data)
        payload.extend(bytes(-len(payload) % 4))
    if len(header) + len(payload) > 2 * 1024**2 - 8:
        raise ValueError("Radio firmware exceeds its two mapped sections")
    output.write_bytes(header + payload)
