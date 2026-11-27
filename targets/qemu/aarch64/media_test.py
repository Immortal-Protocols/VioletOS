"""Windowless VP9/Opus check using Home, the browser, and the actual guest sound card."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import struct
from threading import Thread

BOOKMARK = "/hypertext/WWW/TheProject.html"
PAGE = b"""<html><head><title>VioletOS playback test</title></head>
<body style="background-color:#f3eed4"><h1>Media fixture</h1>
<video controls src="http://media.test/colors.webm"></video></body></html>"""
CLIP = Path(__file__).resolve().parents[1] / "fixtures/colors.webm"


class MediaTest:
    def __init__(self, stack):
        self.requests = set()
        requests = self.requests

        class Site(BaseHTTPRequestHandler):
            def do_GET(self):
                location = (self.headers.get("Host"), self.path)
                requests.add(location)
                if location == ("info.cern.ch", BOOKMARK):
                    body, mime = PAGE, "text/html"
                elif location == ("media.test", "/colors.webm"):
                    body, mime = CLIP.read_bytes(), "video/webm"
                else:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", mime)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Site)
        stack.callback(server.server_close)
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        stack.callback(server.shutdown)
        port = server.server_address[1]
        os.environ["VIOLET_NET_HOSTS"] = json.dumps({name: f"127.0.0.1:{port}" for name in ("info.cern.ch", "media.test")})
        self.stage = "home"
        self.colors = []
        self.tone = 0
        self.frames = 0
        self.sequence = 0

    def __str__(self):
        return f"{self.stage}, requests={sorted(self.requests)}, colors={self.colors}, {self.frames} PCM frames, {self.tone:.0f} Hz"

    def serial(self, text):
        if self.stage == "home" and "ladybird: loaded about:home status=200" in text:
            self.stage = "bookmark"
        if self.stage == "page" and 'title="VioletOS playback test"' in text:
            self.stage = "play link"
        if "Cannot play this WebM" in text or "Audio playback failed" in text:
            raise SystemExit(text)

    @staticmethod
    def frequency(memory, card, head, size):
        n = 2048
        samples = [struct.unpack_from("<h", memory, card + 4096 + 4 * ((head-n+i) % size))[0] for i in range(n)]
        if max(samples) - min(samples) < 100:
            return 0
        edges = [i - a/(b-a) for i, (a,b) in enumerate(zip(samples, samples[1:])) if a <= 0 < b]
        if len(edges) < 10:
            return 0
        return (len(edges)-1) * 48000 / (edges[-1]-edges[0])

    def check(self, memory, scan, card, width, height, tap):
        def pixel(x, y):
            return struct.unpack_from("<I", memory, scan + (y*width+x)*4)[0] & 0xffffff

        if self.stage in ("bookmark", "play link"):
            if self.stage == "play link" and pixel(360, 1400) != 0xf3eed4:
                return False
            # The first hyperlink on the start page opens the CERN bookmark.
            # The fixture uses the one visible hyperlink generated from <video src>.
            seq = struct.unpack_from("<I", memory, scan+width*height*4+12)[0]
            if seq == self.sequence:
                return False
            self.sequence = seq
            for y in range(110, height-50):
                row = memory[scan+y*width*4:scan+(y+1)*width*4]
                at = row.find(bytes.fromhex("c4551aff"))
                if at >= 0 and at % 4 == 0:
                    if self.stage == "bookmark":
                        tap(200, y+12)
                        self.stage = "page"
                    else:
                        tap(at//4+7, y+9)
                        self.stage = "playback"
                    return False
        if self.stage != "playback" or ("media.test", "/colors.webm") not in self.requests:
            return False
        rgb = [pixel(x,y) for x in (330,360,390) for y in (330,350,370)]
        color = None
        if all((p>>16)>180 and ((p>>8)&255)<70 and (p&255)<70 for p in rgb):
            color = "red"
        elif all((p&255)>180 and ((p>>8)&255)<70 and (p>>16)<70 for p in rgb):
            color = "blue"
        if color and (not self.colors or self.colors[-1] != color):
            self.colors.append(color)
        magic = struct.unpack_from("<I", memory, card+8)[0]
        self.frames, rate, size = struct.unpack_from("<III", memory, card+20)
        if magic == 0x31445541 and rate == 48000 and size == 8192 and self.frames >= 2048 and not self.tone:
            hz = self.frequency(memory, card, self.frames, size)
            if 435 <= hz <= 445:
                self.tone = hz
        return len(self.colors) >= 3 and self.frames >= 24000 and bool(self.tone)
