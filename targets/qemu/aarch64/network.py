"""Attach the existing VioletOS socket relay to the buffer assigned by seL4."""
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tools"))
from net_relay import Relay, SLOTS, STRIDE


def start_relay(ram, line):
    match = re.fullmatch(r"VioletOS AArch64 network at 0x([0-9a-f]{16})(?: slots 0x([0-9a-f]{16}))?", line.strip())
    if not match:
        return None
    offset = int(match[1], 16) - 0x40000000
    slots = int(match[2], 16) if match[2] else SLOTS
    if slots not in (8, 64):
        raise ValueError("Unsupported socket ring count")
    data_start = (256 + slots * 128 + 4095) & ~4095
    if offset % 4096 or not 0 <= offset <= ram.stat().st_size - (data_start + slots * STRIDE):
        raise ValueError("Network pages outside guest RAM")
    relay = Relay(ram, offset, slots)
    relay.start()
    return relay
