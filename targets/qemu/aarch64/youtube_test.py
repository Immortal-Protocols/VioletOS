"""Observe public YouTube playback through the native socket, screen and sound devices."""
from collections import deque
from pathlib import Path
import re
import struct
import time
import zlib
from urllib.parse import parse_qs, urlsplit


# The live, unmodified sample embedded in Google's YouTube IFrame API reference.
# Its onReady and onStateChange handlers set the iframe's border color.
IFRAME_SAMPLE = ("https://developers-dot-devsite-v2-prod.appspot.com/youtube/"
                 "iframe_api_reference_b0e63fd04d3918e7222d13a21d1fb4d992e5f9aff4eaf57272ca2903e193174c.frame")


IFRAME_BORDER_STATES = {
    bytes.fromhex(bgr): name for bgr, name in (
        ("4f4737", "unstarted"), ("006dff", "ready"), ("ff00aa", "buffering"),
        ("1e6933", "playing"), ("002cdd", "paused"), ("00ffff", "ended"))
}


class YouTubeTest:
    def __init__(self, image_dir, rect, start_tap):
        args = (Path(image_dir) / "argv.bin").read_bytes().rstrip(b"\0").split(b"\0")
        location = args[-1].decode()
        self.iframe_sample = location == IFRAME_SAMPLE
        url = urlsplit(location)
        video_id = parse_qs(url.query).get("v", [""])[0]
        watch_page = (url.scheme == "https" and url.hostname in ("youtube.com", "www.youtube.com", "m.youtube.com")
                      and url.path == "/watch" and re.fullmatch(r"[\w-]{11}", video_id, re.ASCII))
        if args[0] != b"/apps/ladybird" or not (self.iframe_sample or watch_page):
            raise SystemExit("Build the native desktop with --open and a public YouTube watch URL or Google's IFrame API sample.")
        self.video_id = "M7lc1UVf-VE" if self.iframe_sample else video_id
        self.rect = rect
        self.start_tap = start_tap
        self.iframe_state = None
        self.quadrant_hashes = None
        self.tapped = False
        self.ready = False
        self.previous = []
        self.last_check = self.last_report = 0
        self.poster_since = None
        self.seen_audio = False
        self.pairs = deque()
        self.head = 0
        self.frames_observed = 0
        self.received = [0, 0]
        self.slots = {}
        self.peak_motion = 0
        self.peak_rms = 0
        self.play_seconds = 0

    def __str__(self):
        matched_frames = self.pairs[-1][1]-self.pairs[0][1] if self.pairs else 0
        return (f"YouTube {self.video_id}, page/media received={self.received[0]}/{self.received[1]} bytes, "
                f"max changed samples={self.peak_motion}/192, max AC RMS={self.peak_rms:.0f}, "
                f"total PCM frames={self.frames_observed}, paired samples={len(self.pairs)}, "
                f"continuous picture and sound={self.play_seconds:.1f}s ({matched_frames} PCM frames)")

    def serial(self, line):
        self.ready |= "ladybird: presented upstream compositor frame" in line

    def observe_network(self, relay):
        if relay is None:
            return
        for slot in range(relay.slots):
            pos = 256 + 128 * slot
            seq, command, ack, state, _, _, port, _, _, head = struct.unpack_from("<10I", relay.mem, pos)
            if seq != ack or command != 1 or state not in (2, 3) or port != 443:
                continue
            name = bytes(relay.mem[pos+64:pos+128]).split(b"\0")[0].decode("ascii", "replace").lower()
            if name in ("youtube.com", "www.youtube.com", "m.youtube.com"):
                group = 0
            elif name.endswith((".googlevideo.com", ".c.youtube.com")):
                group = 1
            else:
                continue
            if name in relay.hosts or f"{name}:443" in relay.hosts:
                raise SystemExit(f"The public playback check cannot verify remapped host {name}.")
            last_seq, last_head = self.slots.get(slot, (None, 0))
            self.received[group] += (head - (last_head if last_seq == seq else 0)) & 0xffffffff
            self.slots[slot] = (seq, head)

    def pixels(self, memory, scan, width):
        x, y, w, h = self.rect
        return [struct.unpack_from("3B", memory, scan + 4 * ((y+(2*j+1)*h//24)*width+x+(2*i+1)*w//32))
                for j in range(12) for i in range(16)]

    def iframe_picture_changed(self, memory, scan, width):
        # Caption and control overlays sit below the sampled interior. Sample
        # rows in each quadrant so slight motion in a speaker's face counts.
        x, y, w, h = self.rect
        hashes = []
        for j in range(2):
            for i in range(2):
                checksum = 0
                for row in range(y + j*h//2, y + (j+1)*h//2, 8):
                    start = scan + 4*(row*width + x + i*w//2)
                    checksum = zlib.crc32(memory[start:start+4*(w//2)], checksum)
                hashes.append(checksum)
        changed = self.quadrant_hashes and sum(a != b for a, b in zip(self.quadrant_hashes, hashes)) >= 3
        self.quadrant_hashes = hashes
        return changed

    @staticmethod
    def audibility(memory, card, head, size):
        # Measure AC energy in each stereo channel. A stale head, silence or DC
        # alone cannot establish audio progress.
        total = [0, 0]
        squares = [0, 0]
        count = 256
        for i in range(count):
            frame = (head - 2048 + i*8) % size
            for ch, sample in enumerate(struct.unpack_from("<2h", memory, card+4096+frame*4)):
                total[ch] += sample
                squares[ch] += sample*sample
        return max(max(0, squares[ch]/count - (total[ch]/count)**2)**0.5 for ch in range(2))

    def check(self, memory, scan, card, width, height, tap):
        now = time.monotonic()
        if now - self.last_check < .25:
            return False
        self.last_check = now
        if not self.ready or not self.received[0]:
            return False
        if self.iframe_sample:
            # Four independent pixels around the sample's 640 x 360 iframe.
            colors = {bytes(memory[scan+4*(y*width+x):scan+4*(y*width+x)+3])
                      for x, y in ((18, 142), (653, 142), (18, 497), (653, 497))}
            state = IFRAME_BORDER_STATES.get(next(iter(colors))) if len(colors) == 1 else None
            if state != self.iframe_state:
                self.iframe_state = state
                print(f"Google's embedded player reports {state}", flush=True)
            if state == "ready" and not self.tapped:
                tap(*(self.start_tap or (336, 320)))
                self.tapped = True
                print("Tapped Play in Google's YouTube IFrame API sample", flush=True)
            if state != "playing":
                self.previous = []
                self.pairs.clear()
                self.quadrant_hashes = None
                return False
        pixels = self.pixels(memory, scan, width)
        changed = [i for i, (a, b) in enumerate(zip(pixels, self.previous)) if max(abs(x-y) for x,y in zip(a,b)) >= 16]
        self.previous = pixels
        self.peak_motion = max(self.peak_motion, len(changed))
        quadrants = {(i%16//8, i//16//6) for i in changed}
        textured = len({tuple(c//24 for c in rgb) for rgb in pixels}) >= 12
        if not textured:
            self.poster_since = None
        elif self.poster_since is None:
            self.poster_since = now

        head, rate, size = struct.unpack_from("<3I", memory, card+20)
        rms = 0
        advance = (head-self.head) & 0xffffffff
        if advance and rate == 48000 and size == 8192 and (self.frames_observed or head >= 2048):
            rms = self.audibility(memory, card, head, size)
        self.head = head
        self.frames_observed += advance
        self.peak_rms = max(rms, self.peak_rms)
        self.seen_audio |= rms >= 70
        if (self.start_tap and not self.tapped and not self.seen_audio and
                self.poster_since is not None and now-self.poster_since >= 2):
            tap(*self.start_tap)
            self.tapped = True
            print("Sent the requested tap to the native player", flush=True)

        if self.pairs and now-self.pairs[-1][0] > 2:
            self.pairs.clear()
        motion = (self.iframe_picture_changed(memory, scan, width) if self.iframe_sample
                  else len(changed) >= 12 and len(quadrants) >= 3)
        if textured and motion and rms >= 70:
            self.pairs.append((now, self.frames_observed))
            while len(self.pairs) > 1 and now-self.pairs[1][0] >= 8:
                self.pairs.popleft()
        self.play_seconds = self.pairs[-1][0]-self.pairs[0][0] if self.pairs else 0
        if now-self.last_report >= 12:
            self.last_report = now
            print(self, flush=True)
        return (self.received[0] >= 16384 and self.received[1] >= 262144 and len(self.pairs) >= 10 and
                self.play_seconds >= 6 and self.pairs[-1][1]-self.pairs[0][1] >= 216000)
