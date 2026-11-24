"""Compose the "Mac at home" Violet Shield demo from `test_shield.py --companion mac --record DIR`.

    build/shield/venv/bin/python tools/shield-trainer/make_mac_video.py build/shield/mac-demo

Left: VioletOS in QEMU, standing in for the phone (frames read from guest memory).
Right: Violet Relay running on the Mac (its own window snapshots, about one a
second), with the step being shown and the Open Runtime's log. Writes
DIR/violet-shield-mac.mp4.
"""
import json
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw

sys.path.insert(0, str(Path(__file__).resolve().parent))
from make_video import ACCENT, ALERT, BG, GREEN, INK, QH, QW, font, wrap  # noqa: E402

W, H, FPS = 1920, 1080, 10
PHONE = (60, 130, 396, 880)  # x, y, w, h
MAC = (500, 110, 880, 900)   # the box the Mac window is fitted into


def main(d: Path):
    meta = [json.loads(l) for l in (d / "qemu.jsonl").read_text().splitlines() if l.strip()]
    raw = (d / "qemu.bgra").open("rb")
    frame_bytes = QW * QH * 4
    q0 = float((d / "qemu.start").read_text())
    shots = sorted((float(p.stem), p) for p in (d / "mac").glob("*.png"))
    out = d / "violet-shield-mac.mp4"
    ff = subprocess.Popen(["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(FPS),
                           "-i", "-", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20", str(out)], stdin=subprocess.PIPE)
    title, sub, cap_f, log_f, label_f = font(40), font(24), font(30), font(15, mono=True), font(21)
    duration = meta[-1]["t"] + 1.0
    idx, raw_index, phone, mac_key, mac = 0, -1, None, None, None
    for n in range(int(duration * FPS)):
        t = n / FPS
        while idx + 1 < len(meta) and meta[idx + 1]["t"] <= t:
            idx += 1
        if idx != raw_index:
            raw.seek(idx * frame_bytes)
            data = raw.read(frame_bytes)
            if len(data) == frame_bytes:
                phone = Image.frombuffer("RGBA", (QW, QH), data, "raw", "BGRA", 0, 1).convert("RGB").resize(PHONE[2:], Image.LANCZOS)
            raw_index = idx
        # The viewer's clock starts about when QEMU does; snapshots carry wall-clock times.
        now = q0 + meta[idx]["t"]
        shot = None
        for st, p in shots:
            if st <= now:
                shot = p
            else:
                break
        if shot and shot != mac_key:
            im = Image.open(shot).convert("RGB")
            scale = min(MAC[2] / im.width, MAC[3] / im.height)
            mac = im.resize((round(im.width * scale), round(im.height * scale)), Image.LANCZOS)
            mac_key = shot
        img = Image.new("RGB", (W, H), BG)
        dr = ImageDraw.Draw(img)
        dr.text((60, 36), "Violet Shield", font=title, fill=ACCENT)
        dr.text((340, 50), "your Mac at home guards your phone", font=sub, fill=INK)
        if phone:
            img.paste(phone, PHONE[:2])
        dr.text((PHONE[0], PHONE[1] + PHONE[3] + 12), "The phone · VioletOS on seL4 (QEMU)", font=label_f, fill=INK)
        if mac:
            img.paste(mac, (MAC[0], MAC[1]))
            dr.rectangle([MAC[0] - 1, MAC[1] - 1, MAC[0] + mac.width, MAC[1] + mac.height], outline=(60, 54, 74), width=2)
            dr.text((MAC[0], MAC[1] + mac.height + 12), "The Mac at home · Violet Relay + Shield (EmbeddingGemma 2)", font=label_f, fill=INK)
        m = meta[idx]
        cap = m.get("caption", "").replace("iPhone companion", "Mac").replace("on the iPhone", "on the Mac").replace("on the phone", "on the Mac")
        low = cap.lower()
        color = ALERT if any(k in low for k in ("froze", "frozen", "blocked", "exfil", "secrets", "overwrite")) else (
            GREEN if any(k in low for k in ("benign", "ordinary", "verified")) else INK)
        x0, ly = 1430, 120
        for line in wrap(dr, cap, cap_f, W - x0 - 50):
            dr.text((x0, ly), line, font=cap_f, fill=color)
            ly += 40
        ly = max(ly + 30, 400)
        dr.text((x0, ly), "Open Runtime log", font=label_f, fill=ACCENT)
        ly += 34
        for line in [l for l in m.get("log", []) if "stats:" not in l][-22:]:
            s = line if len(line) <= 50 else line[:49] + "…"
            hit = any(k in line for k in ("shield", "sentinel", "froze", "thawed"))
            dr.text((x0, ly), s, font=log_f, fill=ALERT if "froze" in line else (ACCENT if hit else (170, 164, 186)))
            ly += 22
        dr.text((x0, H - 50), f"t = {t:5.1f} s · real time", font=label_f, fill=(120, 114, 136))
        ff.stdin.write(img.tobytes())
    ff.stdin.close()
    ff.wait()
    print(out)


if __name__ == "__main__":
    main(Path(sys.argv[1]))
