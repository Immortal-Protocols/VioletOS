"""Check the full upstream MediaSource, compositor and AudioServer in the offline guest."""
from pathlib import Path
import base64
import struct
import time

from media_test import CLIP, MediaTest


def write_fixture(path):
    template = Path(__file__).with_name("upstream-media.html").read_text()
    path.write_text(template.replace("@CLIP@", base64.b64encode(CLIP.read_bytes()).decode()))


class UpstreamMediaTest:
    def __init__(self, require_sound=True):
        self.require_sound = require_sound
        self.started = False
        self.seen = []
        self.events = set()
        self.tone = 0
        self.frames = 0

    def __str__(self):
        return f"upstream MSE colors={self.seen}, events={sorted(self.events)}, {self.frames} PCM frames, {self.tone:.0f} Hz"

    def serial(self, line):
        for action in ("opened", "added", "playing", "later"):
            if f"VioletMSE {action}" in line:
                self.events.add(action)
        if "VioletMSE error" in line:
            raise SystemExit(line)

    def check(self, memory, scan, card, width, height, tap):
        b, g, r = struct.unpack_from("3B", memory, scan + (220*width + 180)*4)
        if not self.started and (r, g, b) == (22, 146, 124):
            tap(180, 220)
            self.started = True
            print("Tapped Play in the upstream page", flush=True)
        if not self.started:
            return False
        color = "red" if r > 160 and b < 70 and g < 70 else "blue" if b > 160 and r < 70 and g < 70 else None
        if color and (not self.seen or self.seen[-1] != color):
            self.seen.append(color)
            print(f"Upstream video presented {color} to the VioletOS screen", flush=True)
        if card is not None:
            self.frames, rate, size = struct.unpack_from("<III", memory, card+20)
            if self.frames >= 2048 and rate == 48000 and size == 8192 and not self.tone:
                measured = MediaTest.frequency(memory, card, self.frames, size)
                if 435 <= measured <= 445:
                    self.tone = measured
                    print(f"Measured {measured:.1f} Hz from the guest speaker", flush=True)
        sound_ok = not self.require_sound or (self.tone and self.frames >= 48000)
        return sound_ok and {"opened", "added", "playing", "later"} <= self.events and {"red", "blue"} <= set(self.seen)

class SustainedMediaTest(UpstreamMediaTest):
    """Measure each one-second color block and sample fresh speaker output throughout the clip."""

    def __init__(self):
        super().__init__()
        self.presented = []
        self.tones = []
        self.previous_head = 0
        self.previous_sample = 0

    def __str__(self):
        gaps = [round(b-a, 3) for a, b in zip(self.presented, self.presented[1:])]
        tone_span = self.tones[-1]-self.tones[0] if self.tones else 0
        return f"{super().__str__()}, frame intervals={gaps}s, {len(self.tones)} tone windows over {tone_span:.2f}s"

    def check(self, memory, scan, card, width, height, tap):
        before = len(self.seen)
        ordinary = super().check(memory, scan, card, width, height, tap)
        now = time.monotonic()
        if len(self.seen) > before:
            self.presented.append(now)
        if self.presented and self.frames != self.previous_head and now-self.previous_sample >= .28:
            self.previous_head = self.frames
            self.previous_sample = now
            if self.frames >= 2048 and 435 <= MediaTest.frequency(memory, card, self.frames, 8192) <= 445:
                self.tones.append(now)
        if not ordinary or len(self.presented) < 6 or self.frames < 250000 or len(self.tones) < 12:
            return False
        return (self.tones[-1]-self.tones[0] >= 4.3 and
                max(b-a for a, b in zip(self.tones, self.tones[1:])) <= 1.2 and
                max(b-a for a, b in zip(self.presented, self.presented[1:])) <= 4)
