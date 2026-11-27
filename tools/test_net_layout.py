"""Check that every copy of the open apps' socket layout agrees (make test-unit).

The layout ("NET2") is written by hand in four places: the Open Runtime
(runtime/open/net.rs, slots.rs), Radio (targets/moto-g-pure/ip/sockets.c), the
emulator's relay (tools/net_relay.py) and the supervisor's mappings
(system/supervisor/supervisor.c, for QEMU and for the handset). A build with
one of them out of step would corrupt rings or fault at run time; this reads
the constants out of each source and fails instead. Milliseconds, no build.
"""
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import net_relay


def rust(path, *names):
    text = (ROOT / path).read_text()
    found = {}
    for name in names:
        m = re.search(rf"\bconst {name}: \w+ = (0x[0-9a-fA-F_]+|\d[\d_]*);", text)
        assert m, f"{path}: no const {name}"
        found[name] = int(m.group(1).replace("_", ""), 0)
    return found


def c_enums(path):
    text = (ROOT / path).read_text()
    found = {}
    for body in re.findall(r"enum\s*\{([^}]*)\}", text):
        for item in body.split(","):
            m = re.fullmatch(r"\s*(\w+)\s*=\s*(0x[0-9a-fA-F]+|\d+)\s*", item)
            if m:
                found[m.group(1)] = int(m.group(2), 0)
    return found


def defines(path):
    text = (ROOT / path).read_text()
    return {m.group(1): m.group(2) for m in re.finditer(r"^#define (\w+) (.+)$", text, re.M)}


def main():
    net = rust("runtime/open/net.rs", "BASE", "TX", "RX", "MAGIC", "HEADERS", "HEADER", "NAME", "RINGS_A", "FIRST", "RINGS_B")
    slots = rust("runtime/open/slots.rs", "SLOTS", "RESERVED", "APP_MAX", "APP_FLOOR")
    names = rust("runtime/open/names.rs", "MAX_NAME")
    radio = c_enums("targets/moto-g-pure/ip/sockets.c")
    relay = {k: getattr(net_relay, k) for k in ("TX", "RX", "MAGIC", "HEADERS", "HEADER", "NAME", "RINGS_A", "FIRST", "RINGS_B", "SLOTS")}

    # The same numbers in all three.
    for key in ("TX", "RX", "MAGIC", "HEADERS", "HEADER", "NAME", "RINGS_A", "FIRST", "RINGS_B"):
        assert net[key] == radio[key] == relay[key], f"{key}: net.rs {net[key]:#x}, sockets.c {radio[key]:#x}, net_relay.py {relay[key]:#x}"
    count = slots["SLOTS"]
    assert count == radio["SLOTS"] == relay["SLOTS"], "the slot count differs"
    assert "pub const SLOTS: usize = slots::SLOTS;" in (ROOT / "runtime/open/net.rs").read_text()
    assert radio["MAX_NAME"] == names["MAX_NAME"] == 253 and names["MAX_NAME"] < net["NAME"]
    assert net["MAGIC"] == int.from_bytes(b"NET2", "little")

    # The region: headers, then two runs of rings around the Bluetooth pages.
    stride = net["TX"] + net["RX"]
    hci = rust("runtime/open/hci.rs", "BASE")["BASE"] - net["BASE"]
    bt = rust("targets/moto-g-pure/bt.rs", "BASE")["BASE"] - 0x0B000000
    assert hci == bt == 0x40000, "the Bluetooth pages moved"
    hole = (hci, hci + 0x3000)  # two shared pages and Radio's own
    assert count <= 32, "the slot table is a 32-bit mask"
    assert net["TX"] & (net["TX"] - 1) == 0 and net["RX"] & (net["RX"] - 1) == 0, "ring positions wrap with the counters"
    assert 64 + net["NAME"] <= net["HEADER"]
    assert net["HEADERS"] + count * net["HEADER"] <= net["RINGS_A"]
    assert net["RINGS_A"] + net["FIRST"] * stride <= hole[0]
    assert net["RINGS_B"] >= hole[1]
    end = net["RINGS_B"] + (count - net["FIRST"]) * stride
    assert end == net_relay.SIZE
    used = [(0, net["RINGS_A"] + net["FIRST"] * stride), (net["RINGS_B"], end)]

    # The supervisor maps all of it, in QEMU and on the handset, at matching offsets.
    supervisor = (ROOT / "system/supervisor/supervisor.c").read_text()
    physical = net_relay.PAGE + 0x40000000
    rows = [(int(pa, 16), int(va, 16), 1 << int(bits or 12)) for pa, va, bits in
            re.findall(r"\{OPEN_RT, (0x[0-9a-f]+), (0x[0-9a-f]+), 0(?:, (\d+))?\}", supervisor)]
    rows = [r for r in rows if physical <= r[0] < physical + 0x100000]
    assert rows == sorted(rows), "device pages must be listed in ascending physical order"
    mapped = set()
    for pa, va, size in rows:
        assert pa - physical == va - net["BASE"], f"{pa:#x} is mapped at another offset ({va:#x})"
        assert pa % size == 0 and va % size == 0
        pages = set(range(pa - physical, pa - physical + size, 4096))
        assert not pages & mapped, f"{pa:#x} is mapped twice"
        mapped |= pages
    for start, stop in used:
        missing = [off for off in range(start & ~4095, stop, 4096) if off not in mapped]
        assert not missing, f"QEMU: socket pages not mapped from {missing[0]:#x}"
    assert physical + max(mapped) + 4096 <= 0x49300000, "the socket pages run into the report page"
    assert hci in mapped and hci + 0x1000 in mapped and hci + 0x2000 not in mapped

    loop = re.search(r"for \(unsigned off = 0; off < (0x[0-9a-f]+);\) \{\n\s*if \(off == (0x[0-9a-f]+)\) off = (0x[0-9a-f]+);(.*?)\n  \}", supervisor, re.S)
    assert loop, "the handset's socket mapping loop was not found"
    limit, skip_from, skip_to = (int(loop.group(i), 16) for i in (1, 2, 3))
    assert f"0x{0x0B000000:08x} + off" in loop.group(4) and f"0x{net['BASE']:08x} + off" in loop.group(4)
    assert (skip_from, skip_to) == hole and limit % 0x10000 == 0 and limit >= end
    assert "SOCKETS: *mut u8 = 0x0b00_0000" in (ROOT / "targets/moto-g-pure/radio.rs").read_text()
    # The supervisor says how much it mapped (page 0, word 5) and Radio serves
    # the slots that fit: all of them here, 7 in the 49 pages older ones mapped.
    said = re.search(r"static const unsigned net_mapped\[6\] = \{0, 0, 0, 0, 0, (0x[0-9a-f]+)\};", supervisor)
    assert said and int(said.group(1), 16) == limit, "the supervisor does not say what it mapped"
    assert "if (!off) fill_frame(frame, (const unsigned char *)net_mapped, sizeof net_mapped);" in loop.group(4)
    assert "off >= 0x10000 && !(off & 0xffff)" in loop.group(4), "the control page must be a 4 KiB frame to be filled"

    def fitting(extent):
        offsets = [net["RINGS_A"] + i * stride if i < net["FIRST"] else net["RINGS_B"] + (i - net["FIRST"]) * stride for i in range(count)]
        return sum(1 for o in offsets if o + stride <= extent)

    assert fitting(limit) == count and radio["LEGACY_EXTENT"] == 49 * 4096 and fitting(radio["LEGACY_EXTENT"]) == 7
    assert limit <= radio["MAX_EXTENT"]

    # lwIP has a connection for every slot, and a window that fits the ring.
    lwip = defines("targets/moto-g-pure/ip/lwipopts.h")
    assert int(lwip["MEMP_NUM_TCP_PCB"]) >= count + 2
    # A UDP PCB for every slot, for each source port DNS uses at once (with
    # LWIP_DNS_SECURE's random ports lwIP makes one a port), and for DHCP.
    assert int(lwip["LWIP_DNS_SECURE"]) & 2 and 1 <= int(lwip["DNS_MAX_SOURCE_PORTS"]) <= int(lwip["DNS_TABLE_SIZE"])
    assert int(lwip["MEMP_NUM_UDP_PCB"]) >= count + int(lwip["DNS_MAX_SOURCE_PORTS"]) + 1
    assert int(lwip["MEMP_NUM_TCP_SEG"]) >= count
    # Commands are numbered alike; a listener's connections waiting to be
    # accepted have lwIP connections of their own, and lwIP counts them
    # against the backlog (which drops SYNs past it).
    cmds = rust("runtime/open/net.rs", "CMD_CONNECT", "CMD_CLOSE", "CMD_UDP", "CMD_LISTEN", "CMD_ACCEPT")
    for name, value in cmds.items():
        assert radio[name] == value == getattr(net_relay, name), f"{name} differs"
    assert int(lwip["MEMP_NUM_TCP_PCB"]) >= count + radio["PARKED"] + 2
    assert lwip["TCP_LISTEN_BACKLOG"] == "1" and lwip["SO_REUSE"] == "1"
    loop = rust("runtime/open/loopback.rs", "MAX_BACKLOG")
    assert radio["LISTEN_BACKLOG"] == net_relay.LISTEN_BACKLOG <= loop["MAX_BACKLOG"] and radio["LISTEN_BACKLOG"] <= radio["PARKED"]
    assert 1 <= int(lwip["MEMP_NUM_TCP_PCB_LISTEN"]) <= count
    assert lwip["TCP_WND"] == "(4*TCP_MSS)" and 4 * int(lwip["TCP_MSS"]) <= net["RX"]
    # lwIP's own local ports and the Open Runtime's port 0 picks never meet.
    eph = rust("runtime/open/loopback.rs", "EPHEMERAL")["EPHEMERAL"]
    for proto in ("TCP", "UDP"):
        lo, hi = int(lwip[f"{proto}_LOCAL_PORT_RANGE_START"], 0), int(lwip[f"{proto}_LOCAL_PORT_RANGE_END"], 0)
        assert 1024 <= lo <= hi < eph, f"lwIP's {proto} local ports {lo}-{hi} overlap the runtime's from {eph}"

    # The quotas add up: every app its floor, the system its reserve.
    runtime = (ROOT / "runtime/open/mod.rs").read_text()
    apps = max(int(n) for n in re.findall(r"pub const MAX_APPS: usize = (\d+);", runtime))
    assert slots["RESERVED"] + apps * slots["APP_FLOOR"] <= count
    assert slots["APP_FLOOR"] <= slots["APP_MAX"] <= count - slots["RESERVED"]
    # Names lwIP may be asking for: every app its questions, the Open Runtime
    # its own, and four of lwIP's table left for Radio.
    sockets_c = (ROOT / "targets/moto-g-pure/ip/sockets.c").read_text()
    assert "ASKS = APP_ASKS + ASKS_EACH" in sockets_c
    assert radio["APP_ASKS"] >= apps * radio["ASKS_EACH"]
    assert int(lwip["DNS_TABLE_SIZE"]) >= radio["APP_ASKS"] + radio["ASKS_EACH"] + 4
    print(f"socket layout: {count} slots, {end:#x} bytes, consistent in net.rs, sockets.c, net_relay.py and supervisor.c")


if __name__ == "__main__":
    main()
