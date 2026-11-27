"""Check live YouTube playback in a graphical Ladybird WebDriver session."""

import argparse
from array import array
import base64
from contextlib import suppress
import json
import math
from pathlib import Path
import subprocess
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, urlsplit
from urllib.request import Request, urlopen


VIDEO_STATE = """
const player = document.getElementById('movie_player');
const reject = Array.from(document.querySelectorAll('button')).find(
    button => button.textContent.trim() === 'Reject all');
if (reject && !player?.querySelector('video')) {
    reject.click();
    return null;
}
const video = player?.querySelector('video');
if (!video) return null;
const stats = player.getVideoStats?.();
const quality = video.getVideoPlaybackQuality();
return {
    title: document.title,
    id: player.getVideoData?.()?.video_id,
    ad: player.classList.contains('ad-showing'),
    paused: video.paused,
    ended: video.ended,
    muted: video.muted || video.volume === 0,
    time: video.currentTime,
    width: video.videoWidth,
    height: video.videoHeight,
    frames: quality.totalVideoFrames,
    dropped: quality.droppedVideoFrames,
    error: video.error?.message || stats?.debug_error || null,
};
"""


def request(server, path, data=None, method=None):
    payload = None if data is None else json.dumps(data).encode()
    message = Request(server + path, data=payload, method=method,
                      headers={"Content-Type": "application/json"})
    try:
        with urlopen(message, timeout=40) as response:
            return json.load(response)["value"]
    except HTTPError as error:
        detail = json.load(error)["value"]
        raise RuntimeError(f"WebDriver {path}: {detail.get('message', detail)}") from error


def sample_audio(source, seconds=5):
    """Read the named PulseAudio sink monitor as signed little-endian PCM."""
    with subprocess.Popen(
        ["parec", "--device", source, "--raw", "--format=s16le",
         "--rate=48000", "--channels=2"],
        stdout=subprocess.PIPE,
    ) as capture:
        try:
            data, _ = capture.communicate(timeout=seconds)
        except subprocess.TimeoutExpired:
            capture.terminate()
            data, _ = capture.communicate(timeout=5)
    samples = array("h")
    samples.frombytes(data[:len(data) & ~1])
    if sys.byteorder != "little":
        samples.byteswap()
    if len(samples) < 2 * 48000:
        raise RuntimeError("The audio monitor returned less than one second of PCM")
    peak = max(abs(sample) for sample in samples)
    if peak < 32:
        raise RuntimeError("The audio monitor was silent during playback")
    return 20 * math.log10(peak / 32768)


def verify(server, session, video_id, seconds, startup_timeout, monitor, screenshot):
    deadline = time.monotonic() + startup_timeout
    start = None
    last = None
    while time.monotonic() < deadline:
        state = request(server, session + "/execute/sync", {"script": VIDEO_STATE, "args": []})
        if state is None or state["ad"] or state.get("id") != video_id:
            time.sleep(1)
            continue
        if state["error"]:
            raise RuntimeError(f"YouTube player error after {last}: {state['error']}")
        if not state["width"] or not state["frames"] or state["muted"] or state["paused"]:
            time.sleep(1)
            continue
        last = state
        if start is None:
            start = state
            deadline = time.monotonic() + seconds + 30
            print(f"Playing {state['title']} at {state['width']}×{state['height']}", flush=True)
            if monitor:
                audio_peak = sample_audio(monitor)
                print(f"PCM peak {audio_peak:.1f} dBFS", flush=True)
            continue
        elapsed = state["time"] - start["time"]
        frames = state["frames"] - start["frames"]
        dropped = state["dropped"] - start["dropped"]
        if elapsed >= seconds and frames > dropped >= 0:
            if screenshot:
                image = request(server, session + "/screenshot")
                screenshot.write_bytes(base64.b64decode(image))
            print(f"PASS: advanced {elapsed:.1f} seconds, {frames - dropped} displayed frames, "
                  f"{dropped} dropped, {state['width']}×{state['height']}", flush=True)
            return
        time.sleep(1)
    raise RuntimeError(f"Playback did not advance {seconds:g} seconds. Last playing state: {last}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("url", help="Public YouTube watch URL")
    parser.add_argument("--server", default="http://127.0.0.1:8000", help="Ladybird WebDriver URL")
    parser.add_argument("--seconds", type=float, default=15, help="Required playback duration")
    parser.add_argument("--startup-timeout", type=float, default=75, help="Seconds allowed to start playback")
    parser.add_argument("--audio-monitor", help="PulseAudio monitor source, for example @DEFAULT_MONITOR@")
    parser.add_argument("--screenshot", type=Path, help="Where to save a PNG of the playing video")
    args = parser.parse_args()
    video_url = urlsplit(args.url)
    video_id = parse_qs(video_url.query).get("v", [""])[0]
    if (video_url.scheme != "https" or video_url.netloc != "www.youtube.com"
            or video_url.path != "/watch" or not video_id):
        parser.error("Use a https://www.youtube.com/watch?v=... URL")
    if not (math.isfinite(args.seconds) and args.seconds > 0
            and math.isfinite(args.startup_timeout) and args.startup_timeout > 0):
        parser.error("Durations must be finite and positive")
    server = args.server.rstrip("/")
    connection = request(server, "/session", {"capabilities": {"alwaysMatch": {
        "browserName": "ladybird", "pageLoadStrategy": "eager",
    }}})
    session = "/session/" + connection["sessionId"]
    try:
        request(server, session + "/timeouts", {"pageLoad": 30000})
        request(server, session + "/url", {"url": args.url})
        verify(server, session, video_id, args.seconds, args.startup_timeout,
               args.audio_monitor, args.screenshot)
    finally:
        with suppress(RuntimeError, URLError):
            request(server, session, method="DELETE")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError) as error:
        raise SystemExit(str(error))
