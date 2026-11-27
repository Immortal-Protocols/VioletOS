"""The QEMU model of an A/B bootloader, for full-system updates.

QEMU has no bootloader: tools/run.py hands it the kernel and files directly.
For updates, VioletOS's data disk gets two boot slots and slot metadata after
its first 16 MiB (sectors below 32768 are the storage volume and installed
apps, untouched here):

    sector 32768          "misc": 1 MiB; bootloader_control at byte 2048
    sector 34816          slot A, 64 MiB
    sector 34816 + 131072 slot B, 64 MiB

The metadata is Android's bootloader_control, the record the XT2163DL's own
boot control HAL writes (targets/moto-g-pure/slot.rs explains where that is
known from); the codec below mirrors slot.rs and is tested against the same
golden record. A slot holds a VSL1 container (tools/ota_bundle.py) of the
files QEMU is started with; an update's manifest is in the slot's last
sector, which this model, like a real bootloader on an unlocked phone, does
not check: the phone checked it before it switched (system/package-manager/
ota.rs).

`boot()` does what an AOSP-style bootloader does at power-on: take the
bootable slot of highest priority, spend one of its tries unless it was
marked successful, record its suffix, and load it. A slot that does not load
is a failed attempt; once a trial's tries are spent the other slot boots.

    python3 tools/ota_boot.py provision DISK IMAGE_DIR   # slot A from a build
    python3 tools/ota_boot.py boot DISK OUT_DIR          # pick a slot, unpack it
    python3 tools/ota_boot.py show DISK
"""
from collections import namedtuple
from pathlib import Path
import binascii
import struct
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ota_bundle  # noqa: E402

MAGIC = 0x42414342
ACTIVE_PRIORITY = 15
ACTIVE_TRIES = 6

Geometry = namedtuple("Geometry", "misc control first sectors")
# The layout Storage uses on the QEMU data disk (services/storage/block.rs).
QEMU = Geometry(misc=32768, control=32768 + 4, first=(34816, 34816 + 131072), sectors=131072)
# A small one for the unit tests.
SMALL = Geometry(misc=2048, control=2052, first=(2064, 2064 + 64), sectors=64)


def disk_bytes(g):
    return (g.first[1] + g.sectors) * 512


class Control:
    """bootloader_control: see targets/moto-g-pure/slot.rs for the layout."""

    def __init__(self, raw):
        self.raw = bytearray(raw[:32])

    @classmethod
    def decode(cls, data):
        if len(data) < 32:
            raise ValueError("short bootloader_control")
        raw = bytes(data[:32])
        magic, version = struct.unpack_from("<IB", raw, 4)
        if magic != MAGIC:
            raise ValueError("bootloader_control magic")
        if version != 1:
            raise ValueError("bootloader_control version")
        if binascii.crc32(raw[:28]) != struct.unpack_from("<I", raw, 28)[0]:
            raise ValueError("bootloader_control CRC")
        if raw[9] & 7 != 2:
            raise ValueError("bootloader_control slot count")
        return cls(raw)

    @classmethod
    def initial(cls, current):
        raw = bytearray(32)
        raw[:2] = b"_a" if current == 0 else b"_b"
        struct.pack_into("<IB", raw, 4, MAGIC, 1)
        raw[9] = 2 | (7 << 3)
        c = cls(raw)
        for i in range(2):
            c.put(i, 7, 7, i == current, False)
        return c

    def encode(self):
        struct.pack_into("<I", self.raw, 28, binascii.crc32(bytes(self.raw[:28])))
        return bytes(self.raw)

    def half(self, i):
        return struct.unpack_from("<H", self.raw, 12 + 2 * i)[0]

    def slot(self, i):
        h = self.half(i)
        return {"priority": h & 15, "tries": (h >> 4) & 7, "successful": bool(h & 0x80), "verity": bool(h & 0x100)}

    def put(self, i, priority, tries, successful, verity):
        h = self.half(i) & ~0x1FF
        h |= (priority & 15) | ((tries & 7) << 4) | (int(successful) << 7) | (int(verity) << 8)
        struct.pack_into("<H", self.raw, 12 + 2 * i, h)

    def suffix(self):
        return {b"_a\0\0": 0, b"_b\0\0": 1}.get(bytes(self.raw[:4]))

    def bootable(self, i):
        s = self.slot(i)
        return s["priority"] > 0 and (s["successful"] or s["tries"] > 0)

    def mark_successful(self, i):
        s = self.slot(i)
        if not s["successful"]:
            self.put(i, s["priority"], 1, True, s["verity"])

    def set_unbootable(self, i):
        s = self.slot(i)
        self.put(i, s["priority"], 0, False, s["verity"])

    def set_active(self, i, current):
        for other in range(2):
            s = self.slot(other)
            if other != i and s["priority"] >= ACTIVE_PRIORITY:
                self.put(other, ACTIVE_PRIORITY - 1, s["tries"], s["successful"], s["verity"])
        s = self.slot(i)
        self.put(i, ACTIVE_PRIORITY, ACTIVE_TRIES, s["successful"], s["verity"] and i == current)

    def choose(self):
        best = None
        for i in range(2):
            if self.bootable(i) and (best is None or self.slot(i)["priority"] > self.slot(best)["priority"]):
                best = i
        if best is None:
            return None
        s = self.slot(best)
        if not s["successful"]:
            self.put(best, s["priority"], s["tries"] - 1, False, s["verity"])
        self.raw[:4] = b"_a\0\0" if best == 0 else b"_b\0\0"
        return best

    def describe(self):
        names = "AB"
        parts = []
        for i in range(2):
            s = self.slot(i)
            parts.append(f"{names[i]}: priority {s['priority']} tries {s['tries']}"
                         f"{' successful' if s['successful'] else ''}{'' if self.bootable(i) else ' (unbootable)'}")
        running = self.suffix()
        return f"last booted {names[running] if running is not None else '?'}; " + "; ".join(parts)


def read_control(disk, g=QEMU):
    with Path(disk).open("rb") as f:
        f.seek(g.control * 512)
        return Control.decode(f.read(512))


def write_control(disk, control, g=QEMU):
    with Path(disk).open("r+b") as f:
        f.seek(g.control * 512)
        f.write(control.encode())


def provision(disk, image, geometry=QEMU):
    """Grow `disk` to hold the slots, put `image` in slot A as the proven
    system and leave B unbootable (a factory flash). Data before misc stays."""
    g = geometry
    disk = Path(disk)
    if len(image) > (g.sectors - 1) * 512:
        raise ValueError("slot image larger than a slot")
    if not disk.exists():
        disk.write_bytes(b"")
    with disk.open("r+b") as f:
        f.seek(0, 2)
        if f.tell() < disk_bytes(g):
            f.truncate(disk_bytes(g))
        f.seek(g.misc * 512)
        f.write(bytes((g.first[0] - g.misc) * 512))
        f.seek(g.first[0] * 512)
        f.write(ota_bundle.pad(image))
        f.write(bytes((g.sectors * 512) - len(ota_bundle.pad(image))))
    control = Control.initial(0)
    control.set_unbootable(1)
    write_control(disk, control, g)


def slot_image(disk, slot, g=QEMU):
    """The VSL1 container in `slot`, read the way a bootloader would: from
    its header, never past the slot. ValueError if it does not parse."""
    with Path(disk).open("rb") as f:
        f.seek(g.first[slot] * 512)
        head = f.read(512)
        if head[:4] != ota_bundle.CONTAINER:
            raise ValueError("no slot image")
        count, = struct.unpack_from("<I", head, 4)
        if not 0 < count <= 20:
            raise ValueError("bad slot image header")
        end = max(at + length for _, at, length in
                  (struct.unpack_from("<16s2I", head, 8 + 24 * i) for i in range(count)))
        if end > (g.sectors - 1) * 512:
            raise ValueError("slot image runs past its slot")
        f.seek(g.first[slot] * 512)
        return ota_bundle.unpack(f.read(end))


def boot(disk, out, geometry=QEMU):
    """One power-on: choose a slot, spend a try, unpack it into `out`.
    Returns the slot, or None when the chosen slot did not load (an attempt
    that counted) or nothing is bootable."""
    g = geometry
    control = read_control(disk, g)
    slot = control.choose()
    write_control(disk, control, g)
    if slot is None:
        return None
    try:
        files = slot_image(disk, slot, g)
    except ValueError:
        return None
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    for name in ota_bundle.QEMU_FILES:
        if name not in files:
            return None
    for name, data in files.items():
        if name in ota_bundle.QEMU_FILES:
            (out / name).write_bytes(data)
    return slot


def main(argv):
    if len(argv) == 3 and argv[0] == "provision":
        provision(argv[1], ota_bundle.qemu_image(argv[2]))
        print(read_control(argv[1]).describe())
    elif len(argv) == 3 and argv[0] == "boot":
        slot = boot(argv[1], argv[2])
        print("booting", "AB"[slot] if slot is not None else "nothing (failed attempt)")
    elif len(argv) == 2 and argv[0] == "show":
        print(read_control(argv[1]).describe())
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
