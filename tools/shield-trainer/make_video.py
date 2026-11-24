"""Compose the Violet Shield demo video from a `test_shield.py --record DIR` run.

    build/shield/venv/bin/python tools/shield-trainer/make_video.py build/shield/demo

Left: VioletOS's screen in QEMU (frames read from guest memory). Middle: the
iPhone companion in the iOS Simulator (`simctl io recordVideo`). Right: the
step being shown and the tail of the Open Runtime's log. Writes DIR/violet-shield-demo.mp4.
"""
import json
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

W, H, FPS = 1920, 1080, 10
QW, QH = 360, 800  # recorded frame size (half of 720x1600)
BG = (18, 15, 26)
INK = (238, 234, 248)
ACCENT = (209, 176, 254)
ALERT = (255, 90, 95)
GREEN = (110, 231, 168)


def font(size, mono=False):
    for path in (["/System/Library/Fonts/SFNSMono.ttf", "/System/Library/Fonts/Menlo.ttc"] if mono else
                 ["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/Helvetica.ttc"]):
        try:
            return ImageFont.truetype(path, size)
        except OSError:
            continue
    return ImageFont.load_default()


def wrap(draw, text, f, width):
    words, lines, cur = text.split(), [], ""
    for w in words:
        t = (cur + " " + w).strip()
        if draw.textlength(t, font=f) > width and cur:
            lines.append(cur)
            cur = w
        else:
            cur = t
    if cur:
        lines.append(cur)
    return lines


def main(d: Path):
    meta = [json.loads(l) for l in (d / "qemu.jsonl").read_text().splitlines() if l.strip()]
    raw = (d / "qemu.bgra").open("rb")
    frame_bytes = QW * QH * 4
    q0 = float((d / "qemu.start").read_text())
    ios = d / "ios.mp4"
    i0 = float((d / "ios.start").read_text()) if (d / "ios.start").exists() else q0
    # The scripted viewer starts once QEMU is up; its t=0 is a little after qemu.start.
    duration = meta[-1]["t"] + 1.0
    panel = d / "panel.mp4"
    ff = subprocess.Popen(["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(FPS),
                           "-i", "-", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20", str(panel)], stdin=subprocess.PIPE)
    title, cap_f, log_f, label_f = font(40), font(34), font(17, mono=True), font(22)
    idx, cur = 0, None
    raw_index = -1
    qx, qy, qw, qh = 60, 110, 414, 920  # QEMU slot
    for n in range(int(duration * FPS)):
        t = n / FPS
        while idx + 1 < len(meta) and meta[idx + 1]["t"] <= t:
            idx += 1
        if idx != raw_index:
            raw.seek(idx * frame_bytes)
            data = raw.read(frame_bytes)
            if len(data) == frame_bytes:
                cur = Image.frombuffer("RGBA", (QW, QH), data, "raw", "BGRA", 0, 1).convert("RGB").resize((qw, qh), Image.LANCZOS)
            raw_index = idx
        img = Image.new("RGB", (W, H), BG)
        dr = ImageDraw.Draw(img)
        dr.text((60, 40), "Violet Shield", font=title, fill=ACCENT)
        dr.text((330, 52), "local AI threat monitoring · signed live Shields · VioletOS on seL4", font=label_f, fill=INK)
        dr.text((qx, qy + qh + 12), "VioletOS · seL4 · QEMU", font=label_f, fill=INK)
        dr.text((540, qy + qh + 12), "iPhone companion · iOS Simulator", font=label_f, fill=INK)
        if cur:
            img.paste(cur, (qx, qy))
        dr.rectangle([540, qy, 540 + 425, qy + qh], outline=(60, 54, 74), width=2)
        m = meta[idx]
        x0 = 1010
        y = 120
        cap = m.get("caption", "")
        color = ALERT if any(k in cap.lower() for k in ("froze", "frozen", "blocked", "exfil", "secrets", "hook")) else (GREEN if "benign" in cap.lower() or "ordinary" in cap.lower() else INK)
        for line in wrap(dr, cap, cap_f, W - x0 - 60):
            dr.text((x0, y), line, font=cap_f, fill=color)
            y += 44
        y = max(y + 30, 360)
        dr.text((x0, y), "Open Runtime log (guest memory)", font=label_f, fill=ACCENT)
        y += 36
        for line in m.get("log", [])[-14:]:
            shown = line if len(line) <= 72 else line[:71] + "…"
            hit = any(k in line for k in ("shield", "sentinel", "froze", "thawed"))
            dr.text((x0, y), shown, font=log_f, fill=ALERT if "froze" in line else (ACCENT if hit else (170, 164, 186)))
            y += 25
        dr.text((x0, H - 50), f"t = {t:5.1f} s · real time", font=label_f, fill=(120, 114, 136))
        ff.stdin.write(img.tobytes())
    ff.stdin.close()
    ff.wait()
    out = d / "violet-shield-demo.mp4"
    if ios.exists():
        # Align the Simulator recording with the QEMU timeline.
        first = q0 + 0.0
        offset = max(0.0, (q0 - i0) + (meta[0]["t"] if meta else 0))
        subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", str(panel), "-ss", f"{offset:.2f}", "-i", str(ios),
                        "-filter_complex", f"[1:v]scale=-2:{qh},setpts=PTS-STARTPTS[p];[0:v][p]overlay=540+(425-overlay_w)/2:{qy}:shortest=0:eof_action=repeat[v]",
                        "-map", "[v]", "-t", f"{duration:.1f}", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20", "-r", str(FPS), str(out)], check=True)
        _ = first
    else:
        panel.replace(out)
    print(out)


if __name__ == "__main__":
    main(Path(sys.argv[1]))
