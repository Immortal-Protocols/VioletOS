"""violet: the VioletOS phone for Python apps (screen, touch, text, sound, saved state).

A Python app is one file starting with `#!/bin/python3`, installed into /apps
like any program. The phone runs it with CPython 3.12 (an installed package:
/bin/python3 and /lib, which also holds this module and its font atlas).

    import violet
    from violet import BACKGROUND, PANEL, INK, MUTED, ACCENT

    s = violet.Screen()
    s.fill(BACKGROUND)
    s.header("Counter")                      # "< Home" and the title, as in every app
    s.button(60, 400, 600, 160, "Tap me")
    s.show()
    for e in violet.events():
        if e.down and e.inside(60, 400, 600, 160):
            ...

Text comes from the same Go fonts as the Rust apps, baked at sizes 24, 32, 40,
48, 64, 88 and 120 px (the nearest is used). Text is blended against `bg`
(default: the color of the last full-screen fill), so pass bg= when drawing on
a panel or button of another color.

On a Mac (no /dev/violet), or with VIOLET_FAKE=1, the screen is an in-memory
buffer and events() plays VIOLET_FAKE_EVENTS ("x,y;x,y;..." taps) then ends
the app, so `./build NAME` can run it once before installing.
"""
import json
import mmap
import os
import struct
import sys
import time

W, H = 720, 1600


def rgb(r, g, b):
    return 0xFF000000 | (r << 16) | (g << 8) | b


BACKGROUND = rgb(0x14, 0x0E, 0x1E)
PANEL = rgb(0x2A, 0x20, 0x3C)
INK = rgb(0xEE, 0xEA, 0xF8)
MUTED = rgb(0xA8, 0x9C, 0xC4)
ACCENT = rgb(0xE6, 0x8C, 0xC8)
GREEN = rgb(0x45, 0xC4, 0x8F)
RED = rgb(0xF0, 0x60, 0x60)
BLACK = rgb(0, 0, 0)
WHITE = rgb(0xFF, 0xFF, 0xFF)

#: Where every app's "< Home" button is: x, y, width, height.
HOME_BOX = (28, 34, 160, 58)

FAKE = os.environ.get("VIOLET_FAKE") == "1" or not os.path.exists("/dev/violet/screen")
_EVENT = struct.Struct("<QHHiii")
_PRESENT = struct.Struct("<6I")


def _px(color):
    return struct.pack("<I", color & 0xFFFFFFFF)


# ---- Fonts ----

class _Face:
    __slots__ = ("size", "bold", "ascent", "descent", "glyphs", "cache")

    def __init__(self, size, bold, ascent, descent):
        self.size, self.bold, self.ascent, self.descent = size, bold, ascent, descent
        self.glyphs = {}
        self.cache = {}


_faces = None
_atlas = b""


def _load_faces():
    global _faces, _atlas
    if _faces is not None:
        return _faces
    _faces = []
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "violet_font.bin")
    try:
        with open(path, "rb") as f:
            data = f.read()
    except OSError:
        return _faces
    if data[:4] != b"VFNT":
        return _faces
    (count,) = struct.unpack_from("<H", data, 4)
    at = 6
    entries = []
    for _ in range(count):
        weight, _, size, ascent, descent, n = struct.unpack_from("<BBHhhH", data, at)
        at += 10
        face = _Face(size, weight == 1, ascent, descent)
        for _ in range(n):
            cp, adv, xmin, ymin, w, h, off = struct.unpack_from("<IfhhHHI", data, at)
            at += 20
            face.glyphs[chr(cp)] = (adv, xmin, ymin, w, h, off)
        entries.append(face)
    _atlas = memoryview(data)[at:]
    _faces = entries
    return _faces


def _face(size, bold):
    faces = [f for f in _load_faces() if f.bold == bold]
    if not faces:
        return None
    return min(faces, key=lambda f: abs(f.size - size))


def _blend_table(fg, bg):
    fr, fgg, fb = (fg >> 16) & 255, (fg >> 8) & 255, fg & 255
    br, bgg, bb = (bg >> 16) & 255, (bg >> 8) & 255, bg & 255
    return [bytes((bb + (fb - bb) * a // 255, bgg + (fgg - bgg) * a // 255, br + (fr - br) * a // 255, 255))
            for a in range(256)]


def text_width(s, size=40, bold=False):
    """The width in pixels of s at this size."""
    f = _face(size, bold)
    if f is None:
        return int(len(s) * size * 0.55)
    unknown = f.glyphs.get("?")
    return int(round(sum((f.glyphs.get(c) or unknown)[0] for c in s)))


# ---- The screen ----

class Screen:
    """The 720 x 1600 screen. Draw, then show() what changed."""

    def __init__(self):
        self.w, self.h = W, H
        self.bg = BACKGROUND
        self._header = False
        if FAKE:
            self._fd = None
            self.fb = bytearray(W * H * 4)
        else:
            self._fd = os.open("/dev/violet/screen", os.O_RDWR)
            self.fb = mmap.mmap(self._fd, W * H * 4)
        global _screen
        _screen = self

    # Shapes
    def fill(self, color):
        """Paint the whole screen; text drawn later blends against this color."""
        self.bg = color
        self.fb[:] = _px(color) * (W * H)

    def rect(self, x, y, w, h, color, radius=0):
        """A rectangle, with rounded corners when radius > 0."""
        x, y, w, h = int(x), int(y), int(w), int(h)
        px = _px(color)
        radius = max(0, min(int(radius), w // 2, h // 2))
        fb = self.fb
        for yy in range(max(0, y), min(H, y + h)):
            inset = 0
            if radius:
                dy = min(yy - y, y + h - 1 - yy)
                if dy < radius:
                    d = radius - dy - 0.5
                    inset = int(round(radius - (radius * radius - d * d) ** 0.5))
            x0, x1 = max(0, x + inset), min(W, x + w - inset)
            if x1 > x0:
                at = (yy * W + x0) * 4
                fb[at:at + (x1 - x0) * 4] = px * (x1 - x0)

    def circle(self, cx, cy, r, color):
        """A filled circle."""
        px = _px(color)
        fb = self.fb
        cx, cy, r = float(cx), float(cy), float(r)
        for yy in range(max(0, int(cy - r)), min(H, int(cy + r) + 1)):
            dy = yy + 0.5 - cy
            if abs(dy) > r:
                continue
            half = (r * r - dy * dy) ** 0.5
            x0, x1 = max(0, int(round(cx - half))), min(W, int(round(cx + half)))
            if x1 > x0:
                at = (yy * W + x0) * 4
                fb[at:at + (x1 - x0) * 4] = px * (x1 - x0)

    def line(self, x0, y0, x1, y1, color, width=4):
        """A straight line `width` pixels wide."""
        steps = int(max(abs(x1 - x0), abs(y1 - y0), 1))
        half = width / 2
        for i in range(0, steps + 1, max(1, int(half))):
            t = i / steps
            x, y = x0 + (x1 - x0) * t, y0 + (y1 - y0) * t
            self.rect(x - half, y - half, width, width, color)

    def pixels(self, x, y, w, h, data):
        """Copy raw pixels (w*h*4 bytes, 0xAARRGGBB little-endian) to x, y."""
        for row in range(h):
            yy = y + row
            if 0 <= yy < H:
                at = (yy * W + x) * 4
                self.fb[at:at + w * 4] = data[row * w * 4:(row + 1) * w * 4]

    # Text
    def text(self, x, y, s, size=40, color=INK, bold=False, bg=None):
        """Draw s with its top-left at x, y. Returns the width drawn."""
        f = _face(size, bold)
        if f is None:
            return text_width(s, size, bold)
        bg = self.bg if bg is None else bg
        key = (color, bg)
        rows_for = f.cache.get(key)
        if rows_for is None:
            rows_for = f.cache[key] = {}
        table = None
        unknown = f.glyphs.get("?")
        pen = float(x)
        base = int(y) + f.ascent
        fb = self.fb
        for ch in str(s):
            g = f.glyphs.get(ch) or unknown
            if g is None:
                continue
            adv, xmin, ymin, w, h, off = g
            if w and h:
                rows = rows_for.get(ch)
                if rows is None:
                    if table is None:
                        table = _blend_table(color, bg)
                    cov = _atlas[off:off + w * h]
                    rows = [b"".join([table[a] for a in cov[r * w:(r + 1) * w]]) for r in range(h)]
                    rows_for[ch] = rows
                gx = int(round(pen + xmin))
                gy = base - ymin - h
                c0, c1 = max(0, -gx), min(w, W - gx)
                if c1 > c0:
                    for r in range(h):
                        yy = gy + r
                        if 0 <= yy < H:
                            at = (yy * W + gx + c0) * 4
                            fb[at:at + (c1 - c0) * 4] = rows[r][c0 * 4:c1 * 4]
            pen += adv
        return int(round(pen - x))

    def text_center(self, x, y, w, s, size=40, color=INK, bold=False, bg=None):
        """Draw s centred in the span x .. x + w, top at y."""
        return self.text(x + (w - text_width(s, size, bold)) // 2, y, s, size, color, bold, bg)

    def line_height(self, size=40, bold=False):
        f = _face(size, bold)
        return (f.ascent - f.descent) if f else int(size * 1.2)

    # Widgets
    def button(self, x, y, w, h, label, bg=PANEL, ink=INK, size=40, bold=True):
        """A rounded button with a centred label."""
        self.rect(x, y, w, h, bg, radius=min(22, h // 2))
        lh = self.line_height(size, bold)
        self.text_center(x, y + (h - lh) // 2, w, label, size, ink, bold, bg)

    def header(self, title):
        """The "< Home" button and the title, as every VioletOS app draws them.
        Taps on the Home button then go home by themselves."""
        x, y, w, h = HOME_BOX
        self.button(x, y, w, h, "‹ Home", PANEL, INK, size=24, bold=False)
        self.text(40, 180 - (_face(48, True).ascent if _face(48, True) else 40), title, 48, INK, True)
        self._header = True

    # Showing
    def show(self, y=0, h=H):
        """Put rows y .. y + h on the panel (all of them by default)."""
        if self._fd is not None:
            y, h = max(0, int(y)), int(min(H - max(0, int(y)), h))
            os.write(self._fd, _PRESENT.pack(0, y, W, h, 0, 0))

    def save_ppm(self, path):
        """The screen as a PPM image (used by ./build's check on the Mac)."""
        out = bytearray(b"P6\n%d %d\n255\n" % (W, H))
        fb = bytes(self.fb)
        rgbx = bytearray(W * H * 3)
        rgbx[0::3] = fb[2::4]
        rgbx[1::3] = fb[1::4]
        rgbx[2::3] = fb[0::4]
        with open(path, "wb") as f:
            f.write(out + rgbx)


_screen = None


# ---- Events ----

class Event:
    """A touch or lifecycle event. kind: "down", "move", "up", "key", "focus", "blur" or "quit"."""
    __slots__ = ("kind", "x", "y", "code", "value")

    def __init__(self, kind, x=0, y=0, code=0, value=0):
        self.kind, self.x, self.y, self.code, self.value = kind, x, y, code, value

    down = property(lambda self: self.kind == "down")
    up = property(lambda self: self.kind == "up")
    move = property(lambda self: self.kind == "move")
    focus = property(lambda self: self.kind == "focus")

    def inside(self, x, y, w, h):
        return x <= self.x < x + w and y <= self.y < y + h

    def __repr__(self):
        return f"Event({self.kind}, {self.x}, {self.y})"


_KINDS = {1: "down", 2: "move", 3: "up", 4: "key", 5: "key"}


def _decode(raw):
    _, kind, code, x, y, value = _EVENT.unpack(raw)
    if kind == 7:
        return Event({1: "focus", 2: "blur", 3: "quit"}.get(value, "focus"), value=value)
    return Event(_KINDS.get(kind, "other"), x, y, code, value)


_swallow = False


def _home_tap(e):
    """A press on the header's Home button (and the lift that ends it)."""
    global _swallow
    if _swallow and e.kind in ("move", "up"):
        _swallow = e.kind != "up"
        return True
    _swallow = _screen is not None and _screen._header and e.down and e.inside(*HOME_BOX)
    return _swallow


def events(timeout=None):
    """Touch and lifecycle events, forever. With a timeout (seconds), None is
    yielded whenever that long passes with no event, for clocks and animations.
    Taps on the header's Home button go home without being yielded."""
    if FAKE:
        yield from _fake_events(timeout)
        return
    fd = os.open("/dev/violet/events", os.O_RDONLY)
    import select
    p = select.poll()
    p.register(fd, select.POLLIN)
    while True:
        if timeout is not None and not p.poll(int(timeout * 1000)):
            yield None
            continue
        raw = os.read(fd, _EVENT.size)
        if len(raw) < _EVENT.size:
            continue
        e = _decode(raw)
        if e.kind == "quit":
            return
        if _home_tap(e):
            if e.down:
                home()
            continue
        yield e


def _fake_events(timeout):
    yield Event("focus", value=1)
    spec = os.environ.get("VIOLET_FAKE_EVENTS", "")
    for pair in filter(None, spec.split(";")):
        x, y = (int(v) for v in pair.split(","))
        for kind in ("down", "up"):
            e = Event(kind, x, y)
            if not _home_tap(e):
                yield e
        if timeout is not None:
            yield None
    if timeout is not None:
        for _ in range(3):
            yield None
    out = os.environ.get("VIOLET_FAKE_SCREEN")
    if out and _screen is not None:
        _screen.save_ppm(out)


# ---- The system ----

def sys_command(line):
    """One command to /dev/violet/sys; returns its answer."""
    if FAKE:
        return ""
    fd = os.open("/dev/violet/sys", os.O_RDWR)
    try:
        os.write(fd, (line + "\n").encode())
        return os.read(fd, 4096).decode(errors="replace").strip()
    finally:
        os.close(fd)


def home():
    """Back to the Home screen."""
    sys_command("home")


def _data_dir():
    if FAKE:
        d = os.environ.get("VIOLET_FAKE_DATA") or os.path.join(os.environ.get("TMPDIR", "/tmp"), "violet-data")
        os.makedirs(d, exist_ok=True)
        return d
    return "/data"


def save(name, value):
    """Keep a small JSON value across restarts (/data/NAME, up to 4 KiB)."""
    with open(os.path.join(_data_dir(), name[:16]), "w") as f:
        json.dump(value, f, separators=(",", ":"))


def load(name, default=None):
    """What save(name, ...) kept, or default."""
    try:
        with open(os.path.join(_data_dir(), name[:16])) as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


_audio = None


def play(pcm):
    """Queue 48 kHz stereo 16-bit PCM (bytes) on the speaker."""
    global _audio
    if FAKE:
        return
    if _audio is None:
        _audio = os.open("/dev/violet/audio", os.O_WRONLY)
    sent = 0
    while sent < len(pcm):
        sent += os.write(_audio, pcm[sent:sent + 16384])


def beep(freq=880, ms=120, volume=0.3):
    """A short tone."""
    import math
    n = 48000 * ms // 1000
    amp = int(32767 * max(0.0, min(1.0, volume)))
    fade = max(1, min(n // 4, 480))
    frames = bytearray()
    step = 2 * math.pi * freq / 48000
    for i in range(n):
        env = min(1.0, i / fade, (n - i) / fade)
        s = int(amp * env * math.sin(step * i))
        frames += struct.pack("<hh", s, s)
    play(bytes(frames))


def log(*args):
    """A line in the phone's log (USB: moto-package open-log)."""
    print(*args, flush=True)
