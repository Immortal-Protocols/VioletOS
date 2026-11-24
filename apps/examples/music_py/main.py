#!/bin/python3
"""RFC sketch: PR #13's Music app as a drop-in Python script (zero build step).

Drop this file into /apps/music/main.py and tap it. No VPK packaging, no 128 KB
limit, no hardcoded URLs in Radio, and no MP3 decoder inside the Audio service.
"""
import mmap
import os
import select
import struct
import threading
import urllib.request

W, H = 720, 1600
EVENT_FMT = "<QHHiii"       # InputEvent (24 bytes, runtime/open/abi.rs)
PRESENT_FMT = "<IIIIII"     # PresentRect (24 bytes)

def stream_track(url: str, audio_fd: int) -> None:
    """Fetch any HTTPS track via standard urllib and pour decoded PCM to Audio."""
    with urllib.request.urlopen(url) as resp:
        while chunk := resp.read(32768):
            # Decode MP3 -> 48kHz stereo s16le using a bundled/native decoder module.
            pcm = decode_mp3_chunk(chunk)
            os.write(audio_fd, pcm)

def decode_mp3_chunk(data: bytes) -> bytes:
    raise NotImplementedError("RFC sketch: calls built-in minimp3/ffmpeg binding")

def main() -> None:
    screen_fd = os.open("/dev/violet/screen", os.O_RDWR)
    audio_fd = os.open("/dev/violet/audio", os.O_WRONLY)
    events_fd = os.open("/dev/violet/events", os.O_RDONLY)

    # Zero-copy 720x1600 BGRA8888 framebuffer via standard mmap().
    fb = mmap.mmap(screen_fd, W * H * 4)

    # Draw full-res album cover & UI directly into `fb` (e.g. via PIL / violet_gfx).
    fb[0:W * 120 * 4] = b"\x24\x1a\x1a\xff" * (W * 120)
    os.write(screen_fd, struct.pack(PRESENT_FMT, 0, 0, W, H, 0, 1))

    # Background thread keeps streaming even after user presses Home.
    url = "https://music.example.com/tracks/01.mp3"
    threading.Thread(target=stream_track, args=(url, audio_fd), daemon=True).start()

    # Standard poll/select loop over touch, volume buttons, and lifecycle events.
    while True:
        select.select([events_fd], [], [])
        raw = os.read(events_fd, struct.calcsize(EVENT_FMT))
        _ts, kind, _code, x, y, _val = struct.unpack(EVENT_FMT, raw)
        if kind == 1:  # EVENT_TOUCH_DOWN
            _ = (x, y)  # handle play/pause/seek and present updated rows

if __name__ == "__main__":
    main()
