"""Board descriptions: what is true of one machine, as data.

A board file (targets/board/<name>.json) says which CPU architecture a board
has, where its RAM is, where the boot chain puts each image, and where each
device's registers are and which process may reach them. Code that needs a
board fact takes it from here (generated Rust constants, a C header, the
emulator command line) instead of repeating the number.

    python3 targets/board/board.py list
    python3 targets/board/board.py check                 validate every board, and compare the
                                                         32-bit boards with the code that still
                                                         hard-codes their device tables
    python3 targets/board/board.py rust qemu64 [-o FILE] constants for a Rust crate
    python3 targets/board/board.py c xt2163dl [-o FILE]  the same as a C header
    python3 targets/board/board.py sync                  rewrite the 32-bit boards' device windows
                                                         from system/supervisor/supervisor.c

README.md in this directory says what is and is not generated from these files today.
"""
import argparse
import json
from pathlib import Path
import re
import sys

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
ARCHES = {
    # What each architecture means for the build. One table, read by every
    # script that takes an architecture (image/packages/arch.py re-exports it).
    "aarch32": {"word_bits": 32, "clang_triple": "arm-none-eabi", "rust_target": "armv7a-none-eabi",
                "app_rust_target": "armv7-unknown-linux-musleabihf", "sel4_arch": "aarch32",
                "elf_class": 1, "elf_machine": 40, "emulator": "qemu-system-arm"},
    "aarch64": {"word_bits": 64, "clang_triple": "aarch64-none-elf", "rust_target": "aarch64-unknown-none",
                "app_rust_target": "aarch64-unknown-linux-musl", "sel4_arch": "aarch64",
                "elf_class": 2, "elf_machine": 183, "emulator": "qemu-system-aarch64"},
}


class BoardError(ValueError):
    pass


def number(value, where):
    if isinstance(value, int) and not isinstance(value, bool):
        return value
    if isinstance(value, str) and re.fullmatch(r"0x[0-9a-fA-F]+|\d+", value):
        return int(value, 0)
    raise BoardError(f"{where}: {value!r} is not a number")


def names():
    return sorted(p.stem for p in HERE.glob("*.json"))


def load(name, directory=HERE):
    path = Path(directory) / f"{name}.json"
    if not path.is_file():
        raise BoardError(f"no board named {name} (have: {', '.join(names())})")
    board = json.loads(path.read_text())
    validate(board, name)
    return board


def spans(board):
    """Every physical range the board names, as (label, start, end)."""
    found = []
    for label, region in board["memory"].get("regions", {}).items():
        pa = number(region["pa"], f"memory.regions.{label}.pa")
        found.append((f"region {label}", pa, pa + number(region["size"], f"memory.regions.{label}.size")))
    for label, device in board.get("devices", {}).items():
        pa = number(device["pa"], f"devices.{label}.pa")
        found.append((f"device {label}", pa, pa + number(device["size"], f"devices.{label}.size")))
    return found


def validate(board, name=None):
    """Raise BoardError unless the description is complete and self-consistent."""
    for key in ("name", "title", "arch", "cpu", "cores", "kernel", "memory"):
        if key not in board:
            raise BoardError(f"board has no {key!r}")
    if name is not None and board["name"] != name:
        raise BoardError(f"{name}.json describes a board named {board['name']!r}")
    if board["arch"] not in ARCHES:
        raise BoardError(f"{board['name']}: unknown architecture {board['arch']!r}")
    word = 1 << ARCHES[board["arch"]]["word_bits"]
    memory = board["memory"]
    ram = memory["ram"]
    ram_start = number(ram["pa"], "memory.ram.pa")
    ram_end = ram_start + number(ram["size"], "memory.ram.size")
    kernel_ram = [(number(r["pa"], "kernel_ram.pa"), number(r["size"], "kernel_ram.size")) for r in memory.get("kernel_ram", [])]
    for i, (pa, size) in enumerate(kernel_ram):
        if size <= 0 or pa % 4096 or size % 4096 or not ram_start <= pa < pa + size <= ram_end:
            raise BoardError(f"{board['name']}: kernel_ram[{i}] is not a page-aligned range inside RAM")
        if i and pa < sum(kernel_ram[i - 1]):
            raise BoardError(f"{board['name']}: kernel_ram[{i}] overlaps or precedes the one before it")
    found = spans(board)
    for label, start, end in found:
        if end <= start or start % 4096 or end % 4096:
            raise BoardError(f"{board['name']}: {label} is not a page-aligned, non-empty range")
    # Image regions and exclusive devices must not overlap each other. A
    # window handed to more than one process says so with "shared": true.
    exclusive = [s for s in found if not _shared(board, s[0])]
    for i, (label, start, end) in enumerate(exclusive):
        for other, ostart, oend in exclusive[i + 1:]:
            if start < oend and ostart < end:
                raise BoardError(f"{board['name']}: {label} overlaps {other}")
    for label, region in memory.get("regions", {}).items():
        pa = number(region["pa"], label)
        if not ram_start <= pa < pa + number(region["size"], label) <= ram_end:
            raise BoardError(f"{board['name']}: region {label} is not inside RAM")
        if "va" in region and number(region["va"], label) + number(region["size"], label) > word:
            raise BoardError(f"{board['name']}: region {label} does not fit a {board['arch']} address")
    windows = [(label, w) for label, device in board.get("devices", {}).items() for w in device.get("windows", [])]
    windows += [("supervisor_windows", w) for w in board.get("supervisor_windows", [])]
    for label, window in windows:
        # Every window says how it may be used: read-only or read-write.
        if window.get("rights") not in ("r", "rw"):
            raise BoardError(f"{board['name']}: a window of {label} has no rights (\"r\" or \"rw\")")
        if number(window.get("frames", 1), label) < 1:
            raise BoardError(f"{board['name']}: a window of {label} maps no frames")
    for label, device in board.get("devices", {}).items():
        for window in device.get("windows", []):
            extent = number(window.get("frames", 1), label) << number(window.get("bits", 12), label)
            if number(window.get("offset", 0), label) + extent > number(device["size"], label):
                raise BoardError(f"{board['name']}: a window of device {label} is larger than the device")
            if number(window["va"], label) + number(device["size"], label) > 1 << 32:
                # Every process keeps its device windows below 4 GiB on both
                # architectures, so one driver source serves both.
                raise BoardError(f"{board['name']}: device {label} is mapped above 4 GiB")
    for absent in board.get("absent", []):
        if absent in board.get("devices", {}):
            raise BoardError(f"{board['name']}: {absent} is listed as absent and as a device")
    return board


def _shared(board, label):
    kind, _, name = label.partition(" ")
    return kind == "device" and board["devices"][name].get("shared", False)


def constants(board):
    """[(NAME, value, comment)] for the generated sources, in a stable order."""
    arch = ARCHES[board["arch"]]
    rows = [("WORD_BITS", arch["word_bits"], "address size of the architecture"),
            ("CORES", number(board["cores"], "cores"), "cores the kernel boots"),
            ("TIMER_HZ", number(board["kernel"]["timer_hz"], "kernel.timer_hz"), "generic timer frequency"),
            ("RAM_PA", number(board["memory"]["ram"]["pa"], "ram"), "start of RAM"),
            ("RAM_SIZE", number(board["memory"]["ram"]["size"], "ram"), "bytes of RAM")]
    for label, region in sorted(board["memory"].get("regions", {}).items()):
        rows.append((f"{label.upper()}_PA", number(region["pa"], label), region.get("what", label)))
        rows.append((f"{label.upper()}_SIZE", number(region["size"], label), ""))
        if "va" in region:
            rows.append((f"{label.upper()}_VA", number(region["va"], label), ""))
    for label, device in sorted(board.get("devices", {}).items()):
        rows.append((f"{label.upper()}_PA", number(device["pa"], label), device.get("what", device.get("kind", label))))
        rows.append((f"{label.upper()}_SIZE", number(device["size"], label), ""))
    return rows


def rust(board):
    lines = [f"//! Board constants for {board['name']} ({board['title']}).",
             "//! Generated by targets/board/board.py from the board description: do not edit.",
             f"pub const NAME: &str = {json.dumps(board['name'])};",
             f"pub const ARCH: &str = {json.dumps(board['arch'])};"]
    for name, value, comment in constants(board):
        lines.append(f"pub const {name}: u64 = {value:#x};" + (f" // {comment}" if comment else ""))
    return "\n".join(lines) + "\n"


def c_header(board):
    guard = "VIOLET_BOARD_" + re.sub(r"\W", "_", board["name"]).upper()
    lines = [f"// Board constants for {board['name']} ({board['title']}).",
             "// Generated by targets/board/board.py from the board description: do not edit.",
             "#pragma once", f"#define {guard} 1",
             f"#define BOARD_NAME {json.dumps(board['name'])}"]
    for name, value, comment in constants(board):
        lines.append(f"#define BOARD_{name} {value:#x}ull" + (f" // {comment}" if comment else ""))
    return "\n".join(lines) + "\n"


def emulator(board, loads=(), entry=None):
    """The emulator command line for a board that has one: `loads` is [(path, address)]."""
    spec = board.get("emulator")
    if not spec:
        raise BoardError(f"{board['name']} is not an emulated board")
    command = [spec["binary"], "-machine", spec["machine"], "-cpu", board["cpu"], "-smp", str(board["cores"]),
               "-m", spec["memory"], "-display", "none", "-serial", "stdio", "-monitor", "none"]
    for path, address in loads:
        command += ["-device", f"loader,file={str(path).replace(',', ',,')},addr={address:#x},force-raw=on"]
    if entry is not None:
        command += ["-device", f"loader,addr={entry:#x},cpu-num=0"]
    return command


# ---- The 32-bit boards: device windows still live in supervisor.c ----------

SUPERVISOR = REPO / "system/supervisor/supervisor.c"
_QEMU_ROW = re.compile(r"\{\s*(\w+)\s*,\s*(0x[0-9a-fA-F]+)\s*,\s*(0x[0-9a-fA-F]+)\s*,\s*0\s*(?:,\s*(\d+)\s*)?\}")
# device(owner, pa, va, bits), board_device(owner, pa, va, root_va), map_device(pa, va, bits),
# map_device_rights(pa, va, bits, rights); pa and va may be "base + off" inside an `off` loop.
_CALL = re.compile(r"\b(device|board_device|map_device|map_device_rights)\(\s*(?:([A-Z_][A-Z0-9_]*|0)\s*,\s*)?"
                   r"(0x[0-9a-fA-F]+)\s*(\+\s*off\s*)?,\s*(0x[0-9a-fA-F]+)\s*(\+\s*off\s*)?,\s*(0x[0-9a-fA-F]+|\d+)\s*"
                   r"(?:,\s*(\w+)\s*)?\)")
_LOOP = re.compile(r"\bfor\s*\(\s*unsigned\s+off\s*=\s*([^;]+);\s*off\s*<\s*([^;]+);\s*off\s*\+=\s*([^)]+)\)")
_ASSIGN = re.compile(r"^\s*(?:unsigned\s+)?([A-Za-z_]\w*)\s*=\s*([0-9a-fA-FxXu\s+*/()-]+);\s*$")
_RIGHTS = {None: "rw", "seL4_AllRights": "rw", "seL4_CanRead": "r"}
# The build defines exactly one of these per board (platform/sel4/loader/build.py).
BOARD_MACROS = {"qemu": "VIOLET_QEMU", "xt2163dl": "VIOLET_MOTO"}


def _value(expression, symbols):
    """An integer C expression of literals and known names, or None."""
    text = re.sub(r"\b(0[xX][0-9a-fA-F]+|\d+)[uU]?\b", r"\1", expression.strip())
    names = set(re.findall(r"[A-Za-z_]\w*", re.sub(r"0[xX][0-9a-fA-F]+", "", text)))
    if not names <= set(symbols) or not re.fullmatch(r"[0-9a-fA-FxX\s+*/()A-Za-z_-]+", text):
        return None
    for name in sorted(names, key=len, reverse=True):
        text = re.sub(rf"\b{name}\b", str(symbols[name]), text)
    try:
        return int(eval(compile(text.replace("/", "//"), "<c>", "eval"), {"__builtins__": {}}))
    except (SyntaxError, ZeroDivisionError, TypeError, NameError):
        return None


def _condition(text, macros):
    """True, False or None (cannot tell) for a #if / #elif expression."""
    def defined(name):
        if name in macros:
            return True
        return False if name in BOARD_MACROS.values() else None
    tokens = re.findall(r"defined\s*\(\s*\w+\s*\)|defined\s+\w+|&&|\|\||!|\(|\)|\w+|\S", text)
    position = 0

    def atom():
        nonlocal position
        token = tokens[position] if position < len(tokens) else None
        position += 1
        if token == "!":
            v = atom()
            return None if v is None else not v
        if token == "(":
            v = either()
            position += 1
            return v
        if token and token.startswith("defined"):
            return defined(re.findall(r"\w+", token)[1])
        if token and re.fullmatch(r"\d+", token):
            return token != "0"
        if token and re.fullmatch(r"\w+", token):
            return defined(token)
        raise ValueError(token)

    def both():
        nonlocal position
        v = atom()
        while position < len(tokens) and tokens[position] == "&&":
            position += 1
            w = atom()
            v = False if v is False or w is False else None if v is None or w is None else True
        return v

    def either():
        nonlocal position
        v = both()
        while position < len(tokens) and tokens[position] == "||":
            position += 1
            w = both()
            v = True if v is True or w is True else None if v is None or w is None else False
        return v
    try:
        value = either()
        return value if position == len(tokens) else None
    except (ValueError, IndexError):
        return None


def board_lines(text, board):
    """(line number, code) of every line compiled for `board`, comments removed.

    The preprocessor's state is followed (#if, #ifdef, #ifndef, #elif, #else,
    #endif, defined(), !, &&, ||). The board's own macro is defined and the
    other board's is not; any other condition (a build option such as
    VIOLET_DEVELOPER_KEY) counts as possibly true, so every window some build
    of the board can map is included."""
    macros = {BOARD_MACROS[board]}
    stack = []  # per open #if: [this branch, some branch was True, some branch could have been]
    text = re.sub(r"/\*.*?\*/", lambda m: "\n" * m[0].count("\n"), text, flags=re.S)
    for number, raw in enumerate(text.splitlines(), 1):
        line = re.sub(r"//.*", "", raw)
        directive = re.match(r"\s*#\s*(\w+)\s*(.*)", line)
        if directive:
            kind, rest = directive[1], directive[2].strip()
            if kind in ("if", "ifdef", "ifndef"):
                value = (_condition(rest, macros) if kind == "if" else
                         _condition(f"defined({rest})", macros) if kind == "ifdef" else _condition(f"!defined({rest})", macros))
                stack.append([value, value is True, value is None])
            elif kind == "elif" and stack:
                frame = stack[-1]
                value = False if frame[1] else _condition(rest, macros)
                if frame[2] and value is True:
                    value = None
                stack[-1] = [value, frame[1] or value is True, frame[2] or value is None]
            elif kind == "else" and stack:
                frame = stack[-1]
                value = False if frame[1] else None if frame[2] else True
                stack[-1] = [value, True, frame[2]]
            elif kind == "endif" and stack:
                stack.pop()
            continue
        if any(frame[0] is False for frame in stack):
            continue
        yield number, line


def supervisor_windows(text):
    """Device windows the supervisor source maps, per board: {"qemu": [...], "xt2163dl": [...]}.

    For each board, every line that board's build compiles (board_lines):
    each device(), board_device(), map_device() and map_device_rights() call
    with a literal physical address, and for QEMU also the rows of the
    `devices[]` table. A window is {"owner", "pa", "va", "bits", "frames",
    "rights"}: `frames` is how many consecutive frames of 2^bits a loop over
    `off` maps (its bounds evaluated with the constants the source assigns),
    `rights` "r" or "rw". owner is the process constant's name, "supervisor"
    for a window the supervisor keeps (board_device() also maps its frame into
    the supervisor at its fourth argument), or "context" where the source maps
    into the task it is building at that point. A window whose extent or
    rights cannot be read raises BoardError rather than being skipped.
    """
    windows = {}
    for board in BOARD_MACROS:
        found, symbols, depth, loops, pending = [], {"PAGE": 0x1000}, 0, [], None
        for number, line in board_lines(text, board):
            stripped = line.strip()
            assign = _ASSIGN.match(line)
            if assign and _value(assign[2], symbols) is not None:
                symbols[assign[1]] = _value(assign[2], symbols)
            header = _LOOP.search(line)
            body = line
            if header:
                bounds = [_value(header[i], symbols) for i in (1, 2, 3)]
                rest = line[header.end():].strip()
                loop = (bounds, number)
                if rest.startswith("{"):
                    loops.append((loop, depth + 1))
                elif rest:
                    pending = None
                    body = rest
                    loops.append((loop, "line"))
                else:
                    pending = loop
                    body = ""
            elif pending and stripped:
                loops.append((pending, "line"))
                pending = None
            active = [loop for loop, scope in loops if scope == "line" or depth >= scope]
            for call, owner, pa, pa_off, va, va_off, size, rights in _CALL.findall(body):
                if bool(pa_off) != bool(va_off):
                    raise BoardError(f"supervisor.c:{number}: only one of the addresses moves with off")
                bits = 12 if call == "board_device" else int(size, 0)
                frames, start = 1, 0
                if pa_off:
                    if not active:
                        raise BoardError(f"supervisor.c:{number}: a window at + off outside a loop over off")
                    (first, end, step), _ = active[-1]
                    if None in (first, end, step) or step != 1 << bits or end <= first:
                        raise BoardError(f"supervisor.c:{number}: cannot tell how much the loop maps")
                    start, frames = first, len(range(first, end, step))
                if (rights or None) not in _RIGHTS:
                    raise BoardError(f"supervisor.c:{number}: unknown rights {rights}")
                who = "context" if call.startswith("map_device") else "supervisor" if owner in ("", "0") else owner.lower()
                found.append({"owner": who, "pa": int(pa, 16) + start, "va": int(va, 16) + start, "bits": bits,
                              "frames": frames, "rights": _RIGHTS[rights or None]})
                if call == "board_device":
                    found.append({"owner": "supervisor", "pa": int(pa, 16), "va": int(size, 16), "bits": 12, "frames": 1, "rights": "rw"})
            depth += body.count("{") - body.count("}") if not header else line.count("{") - line.count("}")
            loops = [(loop, scope) for loop, scope in loops if scope != "line" and depth >= scope]
        windows[board] = found
    table = re.search(r"devices\[\]\s*=\s*\{(.*?)\};", text, re.S)
    if table:
        for owner, pa, va, bits in _QEMU_ROW.findall(re.sub(r"//[^\n]*", "", table[1])):
            # Each row's frame is mapped into its process with all rights (wire()).
            windows["qemu"].append({"owner": "supervisor" if owner == "0" else owner.lower(), "pa": int(pa, 16), "va": int(va, 16),
                                    "bits": int(bits or 12), "frames": 1, "rights": "rw"})
    return windows


def window_key(window):
    return (window["pa"], window["owner"], window["va"], window["bits"], window.get("frames", 1), window["rights"])


def described_windows(board):
    rows = []
    for device in board.get("devices", {}).values():
        for window in device.get("windows", []):
            rows.append({"owner": window["owner"], "pa": number(device["pa"], "pa") + number(window.get("offset", 0), "offset"),
                         "va": number(window["va"], "va"), "bits": number(window.get("bits", 12), "bits"),
                         "frames": number(window.get("frames", 1), "frames"), "rights": window["rights"]})
    for window in board.get("supervisor_windows", []):
        rows.append({"owner": window["owner"], "pa": number(window["pa"], "pa"), "va": number(window["va"], "va"),
                     "bits": number(window.get("bits", 12), "bits"), "frames": number(window.get("frames", 1), "frames"),
                     "rights": window["rights"]})
    return rows


def _region(board, name, key):
    return number(board["memory"]["regions"][name][key], name)


# Board facts that code outside this directory still states for itself:
# (boards, what, file, pattern with one group per value, the values the board file gives).
FACTS = [
    (("qemu", "xt2163dl"), "the root task's window", "platform/sel4/loader/build.py", r"ROOT_BYTES = (0x[0-9a-f]+)",
     lambda b: [_region(b, "root", "size")]),
    (("qemu", "xt2163dl"), "the framebuffer", "boot/loader/loader.c", r"fb = (0x[0-9a-f]+), pool = (0x[0-9a-f]+)",
     lambda b: [_region(b, "framebuffer", "pa"), _region(b, "framebuffer", "size")]),
    (("xt2163dl",), "the open apps' memory", "boot/loader/loader.c", r"open_base = (0x[0-9a-f]+)u, open_size = (0x[0-9a-f]+)u",
     lambda b: [_region(b, "open_apps", "pa"), _region(b, "open_apps", "size")]),
    (("xt2163dl",), "the camera's frame memory", "boot/loader/loader.c", r"camera_base = (0x[0-9a-f]+)u, camera_size = (0x[0-9a-f]+)u",
     lambda b: [_region(b, "camera", "pa"), _region(b, "camera", "size")]),
    (("xt2163dl",), "the factory boot image", "platform/sel4/loader/build.py", r'hexdigest\(\)=="([0-9a-f]{64})"',
     lambda b: [b["boot"]["factory_boot_sha256"]]),
    (("xt2163dl",), "the timer frequency", "platform/sel4/board/config.cmake", r"set\(violet_timer_hz (\d+)\)\n    if\(VioletQemuBoard\)",
     lambda b: [number(b["kernel"]["timer_hz"], "timer")]),
    (("qemu",), "the timer frequency", "platform/sel4/board/config.cmake", r"ELLIS_WIFI_FIXTURE ON\)\n        set\(violet_timer_hz (\d+)\)",
     lambda b: [number(b["kernel"]["timer_hz"], "timer")]),
    (("qemu64",), "the timer frequency", "kernel/sel4/aarch64/config.cmake", r"TIMER_FREQUENCY (\d+)",
     lambda b: [number(b["kernel"]["timer_hz"], "timer")]),
    (("qemu",), "the file system image's address", "tools/run.py", r"addr=(0x4a000000),force-raw",
     lambda b: [_region(b, "initfs", "pa")]),
    (("qemu",), "the staging address", "platform/sel4/loader/qemu-entry.S", r"ldr r0, =(0x[0-9a-f]+)\n    ldr r1, =0x40008000",
     lambda b: [_region(b, "staging", "pa")]),
]


def fact_errors(board, repo=REPO):
    """Board facts repeated in code that no longer agree with the board file."""
    errors = []
    for boards, what, file, pattern, expected in FACTS:
        if board["name"] not in boards:
            continue
        path = Path(repo) / file
        match = re.search(pattern, path.read_text()) if path.is_file() else None
        if not match:
            errors.append(f"{board['name']}: {file} no longer states {what} where the board check expects it")
            continue
        found = [g if re.fullmatch(r"[0-9a-f]{64}", g) else int(g, 0) for g in match.groups()]
        if found != expected(board):
            show = lambda values: ", ".join(v if isinstance(v, str) else hex(v) for v in values)
            errors.append(f"{board['name']}: {what} is {show(found)} in {file}, {show(expected(board))} in the board file")
    # Every board on the ellis kernel platform shares its memory reservations.
    dts = Path(repo) / "platform/sel4/board/moto-radio-memory.dts"
    if board["kernel"].get("platform") == "ellis" and dts.is_file():
        cells = [int(c, 16) for c in re.findall(r"0x[0-9a-f]+", dts.read_text().split("reg = <", 1)[1].split(">", 1)[0])]
        kernel_ram = [(cells[i + 1], cells[i + 3]) for i in range(0, len(cells), 4)]
        described = [(number(r["pa"], "pa"), number(r["size"], "size")) for r in board["memory"].get("kernel_ram", [])]
        if kernel_ram != described:
            errors.append(f"{board['name']}: kernel_ram differs from platform/sel4/board/moto-radio-memory.dts")
    return errors


def drift(board, text=None):
    """Differences between a board's description and the code that still states the same facts."""
    errors = fact_errors(board) if text is None else []
    if board["name"] not in ("qemu", "xt2163dl"):
        return errors
    code = supervisor_windows(SUPERVISOR.read_text() if text is None else text)[board["name"]]
    have = sorted(set(map(window_key, described_windows(board))))
    want = sorted(set(map(window_key, code)))
    for pa, owner, va, bits, frames, rights in sorted(set(want) - set(have)):
        errors.append(f"{board['name']}: supervisor.c maps {pa:#x} for {owner} at {va:#x} ({frames} x {bits} bits, {rights}); "
                      "the board file does not say so")
    for pa, owner, va, bits, frames, rights in sorted(set(have) - set(want)):
        errors.append(f"{board['name']}: the board file gives {owner} {pa:#x} at {va:#x} ({frames} x {bits} bits, {rights}); "
                      "supervisor.c does not map it")
    return errors


def sync(name, text=None):
    """Rewrite a 32-bit board's `supervisor_windows` from supervisor.c. Named devices are kept."""
    path = HERE / f"{name}.json"
    board = json.loads(path.read_text())
    code = set(map(window_key, supervisor_windows(SUPERVISOR.read_text() if text is None else text)[name]))
    named = set()
    for device in board.get("devices", {}).values():
        for window in device.get("windows", []):
            place = (number(device["pa"], "pa") + number(window.get("offset", 0), "offset"), window["owner"],
                     number(window["va"], "va"), number(window.get("bits", 12), "bits"))
            # A named window takes its extent and rights from the code when the code maps it.
            match = [key for key in code if key[:4] == place]
            if match:
                window["frames"], window["rights"] = match[0][4], match[0][5]
                named.add(match[0])
    rest = sorted(code - named)
    board["supervisor_windows"] = [{"owner": owner, "pa": f"{pa:#010x}", "va": f"{va:#010x}", "bits": bits, "frames": frames, "rights": rights}
                                   for pa, owner, va, bits, frames, rights in rest]
    path.write_text(json.dumps(board, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["list", "check", "rust", "c", "sync"])
    parser.add_argument("board", nargs="?")
    parser.add_argument("-o", "--output", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "list":
            for name in names():
                board = load(name)
                print(f"{name:10} {board['arch']:8} {board['title']}")
            return
        if args.command == "check":
            errors = []
            for name in names():
                errors += drift(load(name))
            if errors:
                sys.exit("Board descriptions disagree with the code (python3 targets/board/board.py sync):\n  " + "\n  ".join(errors))
            print(f"{len(names())} board descriptions are valid and match the code")
            return
        if args.command == "sync":
            for name in [args.board] if args.board else ["qemu", "xt2163dl"]:
                sync(name)
            return
        if not args.board:
            parser.error("name a board")
        text = (rust if args.command == "rust" else c_header)(load(args.board))
        if args.output:
            args.output.write_text(text)
        else:
            sys.stdout.write(text)
    except BoardError as error:
        sys.exit(f"board: {error}")


if __name__ == "__main__":
    main()
