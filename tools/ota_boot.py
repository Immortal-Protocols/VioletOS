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
files QEMU is started with, and the slot's last sector holds the signed
update manifest (VOT1) for it: what Storage writes when it switches slots
(services/storage/slotwrite.rs), and what `provision` writes for the factory
image.

Like an AVB bootloader, the model checks a slot before it loads it, against
keys and a rollback floor it keeps itself, never ones read from the slot
(the loader and the keys it pins travel in the slot, so whoever writes a
slot would otherwise choose what is trusted):

    misc + 4   bootloader_control (the only misc sector Storage's slot
               interface reaches: services/storage/block.rs slot_sector)
    misc + 5   the rollback floor (tools/boot_manifest.py floor_record): the
               highest version a slot was marked successful with
    misc + 6   the trusted keys ("VBK1"), written once by `provision`, as a
               phone's key is fused at the factory

The slot boots only if its manifest is signed for QEMU by a trusted key, its
digest covers the whole slot image (loader, entry, device tree, /apps), its
version is at least the floor, and the boot manifest the loader carries
(boot/loader/verified.rs) states the same version. A slot that fails is
marked unbootable and the other slot is tried in the same power-on, so a
refused slot can never hold the device while a good one is on disk. The
floor rises only from a slot that passed this check and is marked
successful; the loader then gets the floor as well (OUT_DIR/rollback-floor,
tools/run.py) and checks the kernel and root task against its manifest.

A real bootloader keeps the floor and keys where the system cannot rewrite
them (RPMB or fuses). Here they are disk sectors outside what Storage's slot
interface accepts, so they hold against a compromised package manager or app
using slot writes, not against code that drives the disk directly (Storage
itself, or the host).

`boot()` does what an AOSP-style bootloader does at power-on: take the
bootable slot of highest priority, spend one of its tries unless it was
marked successful, record its suffix, verify and load it; once a trial's
tries are spent, or a slot fails its check, the other slot boots.

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
import boot_manifest  # noqa: E402
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


def read_floor(disk, g=QEMU):
    """The stored rollback floor; ValueError when its record is damaged."""
    with Path(disk).open("rb") as f:
        f.seek((g.control + 1) * 512)
        return boot_manifest.read_floor_record(f.read(512).ljust(16, b"\0"))


def write_floor(disk, floor, g=QEMU):
    with Path(disk).open("r+b") as f:
        f.seek((g.control + 1) * 512)
        f.write(boot_manifest.floor_record(floor).ljust(512, b"\0"))


KEYS_MAGIC = b"VBK1"
POINT = 65


def key_record(keys):
    """The trusted-keys sector: "VBK1" | count u32 | count x (key ID u32,
    P-256 point) | CRC-32 of what precedes it."""
    if not 0 < len(keys) <= 4:
        raise ValueError("the bootloader model trusts 1 to 4 keys")
    body = KEYS_MAGIC + struct.pack("<I", len(keys))
    for key_id, point in sorted(keys.items()):
        if len(point) != POINT or point[0] != 4 or not 0 < key_id < 1 << 32:
            raise ValueError("a trusted key is a key ID and an uncompressed P-256 point")
        body += struct.pack("<I", key_id) + bytes(point)
    return (body + struct.pack("<I", binascii.crc32(body))).ljust(512, b"\0")


def read_key_record(sector):
    """{key_id: point}; ValueError when missing or damaged (nothing may boot)."""
    raw = bytes(sector)
    if raw[:4] != KEYS_MAGIC:
        raise ValueError("no trusted keys provisioned")
    count, = struct.unpack_from("<I", raw, 4)
    if not 0 < count <= 4:
        raise ValueError("trusted keys record")
    end = 8 + count * (4 + POINT)
    if binascii.crc32(raw[:end]) != struct.unpack_from("<I", raw, end)[0]:
        raise ValueError("trusted keys record")
    keys = {}
    for i in range(count):
        at = 8 + i * (4 + POINT)
        keys[struct.unpack_from("<I", raw, at)[0]] = raw[at + 4:at + 4 + POINT]
    return keys


def read_keys(disk, g=QEMU):
    with Path(disk).open("rb") as f:
        f.seek((g.control + 2) * 512)
        return read_key_record(f.read(512).ljust(512, b"\0"))


def provision(disk, image, geometry=QEMU, signer=None):
    """Grow `disk` to hold the slots, put `image` in slot A as the proven
    system with its signed manifest, record the signer's keys as the ones
    this bootloader trusts, and leave B unbootable (a factory flash). The
    signer is this tree's (image/packages/sign.py config) unless given; the
    manifest's version is the image's own. Data before misc stays."""
    g = geometry
    disk = Path(disk)
    if len(image) > (g.sectors - 1) * 512:
        raise ValueError("slot image larger than a slot")
    if signer is None:
        signer = ota_bundle.sign.config(ota_bundle.ROOT, "qemu")
    manifest = ota_bundle.manifest(signer, ota_bundle.image_version(image), ota_bundle.LAYOUT_QEMU, image)
    keys = key_record(signer.keys)
    if not disk.exists():
        disk.write_bytes(b"")
    with disk.open("r+b") as f:
        f.seek(0, 2)
        if f.tell() < disk_bytes(g):
            f.truncate(disk_bytes(g))
        f.seek(g.misc * 512)
        f.write(bytes((g.first[0] - g.misc) * 512))
        f.seek((g.control + 2) * 512)
        f.write(keys)
        f.seek(g.first[0] * 512)
        f.write(ota_bundle.pad(image))
        f.write(bytes((g.sectors * 512) - len(ota_bundle.pad(image))))
        f.seek((g.first[0] + g.sectors - 1) * 512)
        f.write(manifest)
    control = Control.initial(0)
    control.set_unbootable(1)
    write_control(disk, control, g)


def verified_slot(disk, slot, keys, floor, g=QEMU):
    """The files of `slot` and its version, after the bootloader's check
    (see the top of this file); ValueError names the first failure."""
    with Path(disk).open("rb") as f:
        f.seek((g.first[slot] + g.sectors - 1) * 512)
        sector = f.read(512)
        _, version, _, layout, length, _ = ota_bundle.fields(sector)
        if layout != ota_bundle.LAYOUT_QEMU or not 0 < length <= (g.sectors - 1) * 512:
            raise ValueError("slot manifest layout or length")
        f.seek(g.first[slot] * 512)
        image = f.read(len(ota_bundle.pad(bytes(length))))
    ota_bundle.verify(sector + image, keys, "qemu")
    if version < floor:
        raise ValueError(f"slot version {version} is below the rollback floor {floor}")
    files = ota_bundle.unpack(image[:length])
    for name in ota_bundle.QEMU_FILES:
        if name not in files:
            raise ValueError(f"slot image has no {name}")
    inner = boot_manifest.find(files["zImage"])
    if inner is None or inner.version != version:
        raise ValueError("the loader's boot manifest does not state the slot's version")
    return files, version


def boot(disk, out, geometry=QEMU, refusals=None):
    """One power-on: choose a slot, spend a try, verify it, unpack it into
    `out`. A slot that fails its check is marked unbootable and the other is
    tried. Returns the slot, or None when nothing bootable passes (or the
    floor or keys are unreadable: then no try is spent). Each refusal's
    reason is appended to `refusals` when given."""
    g = geometry
    try:
        # An unreadable floor or key record: nothing boots, no try is spent.
        floor = read_floor(disk, g)
        keys = read_keys(disk, g)
    except ValueError as e:
        if refusals is not None:
            refusals.append(str(e))
        return None
    control = read_control(disk, g)
    for _ in range(2):
        slot = control.choose()
        write_control(disk, control, g)
        if slot is None:
            return None
        try:
            files, version = verified_slot(disk, slot, keys, floor, g)
        except ValueError as e:
            if refusals is not None:
                refusals.append(f"slot {'AB'[slot]}: {e}")
            control.set_unbootable(slot)
            write_control(disk, control, g)
            continue
        # Verified here and marked successful (it ran healthy): nothing older boots again.
        if control.slot(slot)["successful"] and version > floor:
            floor = version
            write_floor(disk, floor, g)
        out = Path(out)
        out.mkdir(parents=True, exist_ok=True)
        (out / "rollback-floor").write_text(f"{floor}\n")
        for name in ota_bundle.QEMU_FILES:
            (out / name).write_bytes(files[name])
        return slot
    return None


def main(argv):
    if len(argv) == 3 and argv[0] == "provision":
        provision(argv[1], ota_bundle.qemu_image(argv[2]))
        print(read_control(argv[1]).describe())
    elif len(argv) == 3 and argv[0] == "boot":
        refusals = []
        slot = boot(argv[1], argv[2], refusals=refusals)
        for reason in refusals:
            print("refused:", reason)
        print("booting", "AB"[slot] if slot is not None else "nothing (failed attempt)")
    elif len(argv) == 2 and argv[0] == "show":
        print(read_control(argv[1]).describe() + f"; rollback floor {read_floor(argv[1])}")
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
