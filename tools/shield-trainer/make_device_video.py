"""Compose the real-device Violet Shield demo from a `test_shield_device.py --record DIR` run.

    build/shield/venv/bin/python tools/shield-trainer/make_device_video.py build/shield/device-demo

Left: the Moto's screen (USB screenshots, `moto-package screenshot`). Middle: the
iPhone's screen (`devicectl device capture screenshot`). Right: the step being shown
and the Moto's Open Runtime log at that moment. Both screens are real captures, about
one every 2 s, shown on the run's own clock; quiet stretches are sped up and marked.
Writes DIR/violet-shield-devices.mp4.
"""
import json
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw

sys.path.insert(0, str(Path(__file__).resolve().parent))
from make_video import ACCENT, ALERT, BG, GREEN, INK, font, wrap  # noqa: E402

W, H, FPS = 1920, 1080, 10
SLOT_Y, SLOT_H = 110, 900


def shots(folder: Path):
    """[(t, path)] sorted by the time in each file's name."""
    out = []
    for p in folder.glob("*"):
        try:
            out.append((float(p.stem), p))
        except ValueError:
            pass
    return sorted(out)


def at(seq, t):
    """The last item at or before t (or the first)."""
    best = seq[0] if seq else None
    for item in seq:
        if item[0] <= t:
            best = item
        else:
            break
    return best


def log_lines(text: str):
    """Moto log lines that matter for the story, without the once-a-second stats."""
    return [l for l in text.splitlines() if l.strip() and "stats:" not in l and "weather: Network unreachable" not in l]


def main(d: Path):
    events = [json.loads(l) for l in (d / "events.jsonl").read_text().splitlines() if l.strip()]
    moto, phone = shots(d / "moto"), shots(d / "iphone")
    log = log_lines((d / "moto-log.txt").read_text(errors="replace"))
    timed = []
    if (d / "moto-log.jsonl").exists():
        rows = [json.loads(l) for l in (d / "moto-log.jsonl").read_text().splitlines() if l.strip()]
        keep = set(log_lines("\n".join(r["line"] for r in rows)))
        timed = [(r["t"], r["line"]) for r in rows if r["line"] in keep]
    end = max([e["t"] for e in events] + [s[0] for s in moto + phone]) + 2
    # Speed: real time around the action, 4x while waiting for the phones to link.
    linked = next((e["t"] for e in events if "linked" in e["caption"]), 0.0)
    start = max(0.0, linked - 20)
    out = d / "violet-shield-devices.mp4"
    ff = subprocess.Popen(["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(FPS),
                           "-i", "-", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20", str(out)], stdin=subprocess.PIPE)
    title, cap_f, log_f, label_f = font(40), font(34), font(17, mono=True), font(22)
    cache = {}

    def load(path: Path, h: int):
        key = (path, h)
        if key not in cache:
            cache.clear()
            im = Image.open(path).convert("RGB")
            cache[key] = im.resize((round(im.width * h / im.height), h), Image.LANCZOS)
        return cache[key]

    t = start
    while t < end:
        fast = t < linked
        img = Image.new("RGB", (W, H), BG)
        dr = ImageDraw.Draw(img)
        dr.text((60, 40), "Violet Shield", font=title, fill=ACCENT)
        dr.text((330, 52), "on real phones · Moto G Pure running VioletOS (seL4) · iPhone 18 Pro Max companion", font=label_f, fill=INK)
        m = at(moto, t)
        if m:
            im = load(m[1], SLOT_H)
            img.paste(im, (60, SLOT_Y))
        dr.text((60, SLOT_Y + SLOT_H + 12), "Moto G Pure · VioletOS · USB screenshot", font=label_f, fill=INK)
        p = at(phone, t)
        if p:
            im = load(p[1], SLOT_H)
            img.paste(im, (540, SLOT_Y))
        dr.text((540, SLOT_Y + SLOT_H + 12), "iPhone · Violet Relay + Shield", font=label_f, fill=INK)
        e = at([(x["t"], x) for x in events], t)
        cap = e[1]["caption"] if e and e[0] <= t else ""
        x0, y = 1010, 120
        low = cap.lower()
        color = ALERT if any(k in low for k in ("froze", "frozen", "exfil", "secrets", "overwrite", "uploads")) else (
            GREEN if any(k in low for k in ("benign", "ordinary", "verified", "linked")) else INK)
        for line in wrap(dr, cap, cap_f, W - x0 - 60):
            dr.text((x0, y), line, font=cap_f, fill=color)
            y += 44
        y = max(y + 30, 380)
        dr.text((x0, y), "Moto: Open Runtime log (read over USB)", font=label_f, fill=ACCENT)
        y += 36
        if timed:
            upto = [l for (lt, l) in timed if lt <= t]
        else:  # older recordings: the whole log, spread in proportion to elapsed time
            upto = log[:max(1, int(len(log) * min(1.0, (t - start) / max(1.0, end - start))))]
        for line in upto[-14:]:
            s = line if len(line) <= 72 else line[:71] + "…"
            hit = any(k in line for k in ("shield", "sentinel", "froze", "thawed"))
            dr.text((x0, y), s, font=log_f, fill=ALERT if "froze" in line else (ACCENT if hit else (170, 164, 186)))
            y += 25
        dr.text((x0, H - 50), f"t = {t:5.1f} s · {'4x' if fast else 'real time'}", font=label_f, fill=(120, 114, 136))
        ff.stdin.write(img.tobytes())
        t += (4.0 if fast else 1.0) / FPS
    ff.stdin.close()
    ff.wait()
    print(out)


if __name__ == "__main__":
    main(Path(sys.argv[1]))
