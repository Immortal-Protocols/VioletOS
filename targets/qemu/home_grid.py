"""Where Home (apps/open/home/src/ui.rs) draws each app's icon, for tests that
tap them: the built-in apps in Home's order, then the image's other titled apps."""

BUILT_IN = ["ladybird", "bitchat", "agent", "music", "settings"]
MAX_APPS = 15


def titled(cpio):
    """The names in the image's apps/.home, in order."""
    at = 0
    while cpio[at:at + 6] == b"070701":
        size, namesize = int(cpio[at + 54:at + 62], 16), int(cpio[at + 94:at + 102], 16)
        name = cpio[at + 110:at + 110 + namesize - 1].decode()
        data = (at + 110 + namesize + 3) & ~3
        if name == "TRAILER!!!":
            break
        if name == "apps/.home":
            return [l.split("\t")[0] for l in cpio[data:data + size].decode().splitlines() if "\t" in l]
        at = (data + size + 3) & ~3
    return []


def apps(cpio):
    listed = titled(cpio)
    others = [n for n in listed if n not in BUILT_IN and n != "home"]
    return ([n for n in BUILT_IN if n in listed] + others)[:MAX_APPS]


def icon(index, count):
    """The centre of icon `index` of `count`, in screen pixels."""
    pitch = 286 if count <= 12 else 232
    return [132, 360, 588][index % 3], 410 + (index // 3) * pitch


# Helpers for the scripted viewers (tools/run.py runs them): put after the
# script's imports. They use the script's own tap(), until() and everything().
VIEWER = r'''
import json as _json, os as _os
HOME_ICONS = _json.loads(_os.environ.get("HOME_ICONS", "{}"))
def home_shown():
    """How many times Home has come to the front (it logs each time)."""
    return everything().count("home: shown") + everything().count("home: ready")
def leave_to_home(press=lambda: tap(100, 60), timeout=10):
    """Do `press` (an app's ‹ Home button by default) and wait for Home."""
    n = home_shown()
    press()
    until("back on Home", lambda: home_shown() > n, timeout)
def open_icon(name):
    """Tap an app's icon on Home."""
    if name not in HOME_ICONS:
        fail(f"{name} is not on Home")
    tap(*HOME_ICONS[name])
'''


def viewer(script, cpio):
    """`script` with VIEWER after its imports, and the icons in its environment."""
    import json
    import os
    names = apps(cpio)
    os.environ["HOME_ICONS"] = json.dumps({n: icon(i, len(names)) for i, n in enumerate(names)})
    lines = script.splitlines(keepends=True)
    at = max(i for i, l in enumerate(lines) if l.startswith("import ")) + 1
    return "".join(lines[:at]) + VIEWER + "".join(lines[at:])
