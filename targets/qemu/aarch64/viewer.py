"""Run the native 64-bit VioletOS graphical guest in its own QEMU instance.

Use --test home or --test ladybird for windowless pixel and touch checks with the graphical
file bundle, or --viewer PATH for the existing VioletOS SDL viewer.
"""
import argparse
from collections import deque
import contextlib
import mmap
import os
from pathlib import Path
import selectors
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import zlib

from image import Elf, LOADER_ADDRESS, RAM_BASE, RAM_MIB, RAM_SIZE

BASE = RAM_BASE
WIDTH, HEIGHT = 720, 1600
PIXELS = WIDTH * HEIGHT * 4
HERE = Path(__file__).resolve().parents[3]


def refill_entropy(memory, entropy):
    head, tail = struct.unpack_from("<II", memory, entropy)
    remaining = (head - tail) & 0xffffffff
    if remaining <= 8192:
        count = 16384 - remaining
        pos = head % 16384
        first = min(count, 16384 - pos)
        random = os.urandom(count)
        base = entropy + 12
        memory[base+pos:base+pos+first] = random[:first]
        memory[base:base+count-first] = random[first:]
        struct.pack_into("<I", memory, entropy, (head+count) & 0xffffffff)
        struct.pack_into("<I", memory, entropy+8, 1)


def positions(out, with_entropy=False):
    root, kernel = Elf(out / "root.elf"), Elf(out / "kernel/kernel.elf")
    root_va = min(v for v, _, _ in root.loads) & -4096
    root_pa = (max(p + len(d) for _, p, d in kernel.loads) + 4095) & -4096
    offset_base = root_pa - root_va - BASE
    scanout = offset_base + root.symbols["display_bridge"]
    card = offset_base + root.symbols["audio_bridge"] if "audio_bridge" in root.symbols else None
    # Root load segments end the boot image in their ELF order.
    offset = (out / "violet.img").stat().st_size - sum(len(d) for _, _, d in root.loads)
    seed = root.symbols["boot_entropy"]
    for va, _, data in root.loads:
        if va <= seed and seed + 4112 <= va + len(data):
            result = (scanout, card, offset + seed - va)
            if with_entropy:
                ring = root.symbols.get("entropy_channel")
                return (*result, None if ring is None else offset_base + ring)
            return result
        offset += len(data)
    raise ValueError("Boot entropy lies outside the ELF")


def report_write(memory, offset, report):
    head, tail = struct.unpack_from("<II", memory, offset)
    if (head - tail) & 0xffffffff >= 64:
        return False
    start = offset + 8 + head % 64 * 6
    memory[start:start+6] = report
    struct.pack_into("<I", memory, offset, (head + 1) & 0xffffffff)
    return True


def screenshot(path, pixels):
    def chunk(kind, content):
        item = kind + content
        return struct.pack(">I", len(content)) + item + struct.pack(">I", zlib.crc32(item))
    rows = bytearray()
    for y in range(HEIGHT):
        row = pixels[y * WIDTH * 4:(y+1) * WIDTH * 4]
        rgb = bytearray(WIDTH * 3)
        rgb[0::3], rgb[1::3], rgb[2::3] = row[2::4], row[1::4], row[0::4]
        rows.extend(b"\0" + rgb)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">2I5B", WIDTH, HEIGHT, 8, 2, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def stop(proc):
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=4)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=HERE / "build/sel4/qemu-aarch64")
    args_mode = parser.add_mutually_exclusive_group(required=True)
    args_mode.add_argument("--test", choices=("home", "ladybird", "session", "media", "audio-mixer", "presentation", "upstream-frames", "upstream-media", "upstream-duration", "youtube"), nargs="?", const="home",
                           help="verify the booted graphical ELF renders and receives a touch")
    args_mode.add_argument("--expect", help="wait for this guest serial text without touching the framebuffer")
    args_mode.add_argument("--viewer", type=Path, help="VioletOS SDL viewer built by make")
    parser.add_argument("--show-after-playback", type=Path, metavar="VIEWER",
                        help="open the same live guest for interaction once its public playback check passes")
    parser.add_argument("--screenshot", type=Path, help="save the guest framebuffer as a PNG")
    parser.add_argument("--timeout", type=float, default=90, help="seconds allowed for the headless check")
    parser.add_argument("--player-rect", type=int, nargs=4, metavar=("LEFT", "TOP", "WIDTH", "HEIGHT"),
                        help="sample this native YouTube video rectangle (default 70 240 580 285)")
    parser.add_argument("--player-tap", type=int, nargs=2, metavar=("X", "Y"),
                        help="send one native click at this player coordinate after the picture appears")
    args = parser.parse_args()
    if args.show_after_playback and args.test != "youtube":
        parser.error("--show-after-playback requires --test youtube")
    if (args.player_rect or args.player_tap) and args.test != "youtube":
        parser.error("player geometry and taps require --test youtube")
    rect = args.player_rect or (70, 240, 580, 285)
    left, top, w, h = rect
    if min(left, top) < 0 or min(w, h) < 16 or left+w > WIDTH or top+h > HEIGHT:
        parser.error("the player rectangle must be at least 16 pixels per side and inside the guest screen")
    if args.player_tap:
        x, y = args.player_tap
        if not (left <= x < left+w and top <= y < top+h):
            parser.error("choose a tap inside the player rectangle")
    scan, card, seed, entropy = positions(args.out, with_entropy=True)
    dirty, input_offset = scan + PIXELS, scan + PIXELS + 4096
    try:
        from network import start_relay
    except ModuleNotFoundError as exc:
        if exc.name != "network":
            raise
        start_relay = None

    with tempfile.TemporaryDirectory(prefix="violet-arm64-") as temp, contextlib.ExitStack() as stack:
        media = None
        if args.test == "media":
            if card is None:
                raise SystemExit("Rebuild this Home image with AArch64 audio and --network")
            from media_test import MediaTest
            media = MediaTest(stack)
        if args.test in ("upstream-frames", "upstream-media"):
            from upstream_media import UpstreamMediaTest
            media = UpstreamMediaTest(require_sound=args.test == "upstream-media")
        if args.test == "upstream-duration":
            if card is None:
                parser.error("the duration measurement needs a VioletOS speaker")
            from upstream_media import SustainedMediaTest
            media = SustainedMediaTest()
        if args.test == "youtube":
            if card is None:
                parser.error("the public playback check needs an image with the VioletOS speaker")
            from youtube_test import YouTubeTest
            media = YouTubeTest(args.out, rect, args.player_tap)
        ram_path, image = Path(temp) / "guest.ram", Path(temp) / "boot.img"
        shutil.copyfile(args.out / "violet.img", image)
        with image.open("r+b") as f:
            f.seek(seed)
            f.write(b"VOSRAND1" + time.time_ns().to_bytes(8, "little") + os.urandom(4096))
        ram = stack.enter_context(ram_path.open("w+b"))
        ram.truncate(RAM_SIZE)
        memory = stack.enter_context(mmap.mmap(ram.fileno(), RAM_SIZE))
        guest = subprocess.Popen(["qemu-system-aarch64", "-machine", "virt,virtualization=on,accel=tcg,memory-backend=ram",
            "-cpu", "neoverse-n1", "-m", f"{RAM_MIB}M", "-smp", "1", "-display", "none", "-monitor", "none", "-nic", "none",
            "-serial", "stdio", "-object", f"memory-backend-file,id=ram,size={RAM_SIZE},mem-path={ram_path},share=on",
            "-device", f"loader,file={image},addr={LOADER_ADDRESS:#x},cpu-num=0"], stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        stack.callback(stop, guest)
        sel = stack.enter_context(selectors.DefaultSelector())
        sel.register(guest.stdout, selectors.EVENT_READ, "serial")
        def open_viewer(executable):
            command = [str(executable.resolve()), str(ram_path), str(scan)]
            if card is not None:
                command.append(str(card))
            window = subprocess.Popen(command, stdout=subprocess.PIPE)
            stack.callback(stop, window)
            sel.register(window.stdout, selectors.EVENT_READ, "touch")
            return window
        window = open_viewer(args.viewer) if args.viewer else None
        reports = deque()
        def tap(x, y):
            reports.extend((bytes([9, x >> 4, y >> 4, ((x & 15) << 4) | (y & 15), 1, 1]), bytes(6)))

        serial_tail = b""
        pending = b""
        deadline = time.monotonic() + args.timeout
        entropy_ready = False
        relay = None
        mixer_done = False
        mixer_tone = 0
        app_ready = tapped = activated = False
        browser_loaded = home_tapped = home_restored = False
        home_pixels = None
        while guest.poll() is None and (window is None or window.poll() is None):
            if (args.test or args.expect) and time.monotonic() > deadline:
                if args.screenshot:
                    screenshot(args.screenshot, memory[scan:scan+PIXELS])
                raise SystemExit(f"AArch64 guest timed out waiting for {media or args.test or args.expect}")
            for key, _ in sel.select(.025):
                data = os.read(key.fileobj.fileno(), 16384)
                if key.data == "serial":
                    sys.stdout.buffer.write(data)
                    sys.stdout.buffer.flush()
                    serial_tail += data
                    lines = serial_tail.split(b"\n")
                    serial_tail = lines.pop()
                    for raw in lines:
                        line = raw.decode(errors="replace").rstrip("\r")
                        if line == "VioletOS AArch64 Open Runtime" and entropy is not None:
                            entropy_ready = True
                        if args.expect and args.expect in line:
                            return
                        if args.expect and "VioletOS AArch64: exit " in line:
                            raise SystemExit(f"Guest exited before {args.expect!r}")
                        if args.test == "audio-mixer":
                            mixer_done |= "upstream AudioServer mixed and drained 48000 frames" in line
                            if "VioletOS AArch64: exit " in line and not mixer_done:
                                raise SystemExit(f"The upstream audio probe failed: {line}")
                        if media:
                            media.serial(line)
                        if args.test == "session":
                            browser_loaded |= "ladybird: loaded about:home status=200" in line
                            home_restored |= home_tapped and "home: shown" in line
                        if args.test in ("home", "session", "media"):
                            app_ready |= "home: ready" in line
                            activated |= "home: opening ladybird" in line
                        elif args.test == "ladybird":
                            app_ready |= "ladybird: loaded about:home status=200" in line
                            activated |= "ladybird: editing address" in line
                        if relay is None and start_relay:
                            relay = start_relay(ram_path, line)
                            if relay:
                                stack.callback(relay.stop)
                        if (args.test or args.expect) and any(s in line for s in ("unhandled page fault", "guest exception", "panicked at")):
                            raise SystemExit("Guest faulted during the graphics test")
                else:
                    pending += data
                    while len(pending) >= 6:
                        reports.append(pending[:6])
                        pending = pending[6:]
            if entropy_ready:
                refill_entropy(memory, entropy)
            if args.test and app_ready and not tapped:
                magic, _, _, seq = struct.unpack_from("<4I", memory, dirty)
                if magic == 0x54524944 and seq:
                    pixels = memory[scan:scan+PIXELS]
                    if args.test == "ladybird" and pixels.count(bytes.fromhex("c4551a")) < 100:
                        continue  # The compositor has not presented the start page links yet.
                    colors = {struct.unpack_from("<I", pixels, n)[0] for n in range(0, PIXELS, 4*127)}
                    if len(colors) < 32:
                        raise SystemExit(f"{args.test} presented only {len(colors)} sampled colors")
                    if args.test == "session":
                        home_pixels = pixels[::431]
                    if args.screenshot and args.test not in ("session", "media"):
                        screenshot(args.screenshot, pixels)
                    print(f"AArch64 {args.test} presented {WIDTH}x{HEIGHT} pixels ({len(colors)} sampled colors)", flush=True)
                    # The Home+Ladybird image has one launchable icon at (144, 407).
                    x, y = (500, 63) if args.test == "ladybird" else (144, 407)
                    tap(x, y)
                    tapped = True
            if args.test == "audio-mixer":
                if card is None:
                    raise SystemExit("Boot the upstream AudioServer mixer probe on a guest with a speaker")
                frames, rate, size = struct.unpack_from("<III", memory, card+20)
                if frames >= 2048 and rate == 48000 and size == 8192 and not mixer_tone:
                    from media_test import MediaTest
                    hz = MediaTest.frequency(memory, card, frames, size)
                    if 435 <= hz <= 445:
                        mixer_tone = hz
                if mixer_done and mixer_tone and frames >= 48000:
                    print(f"Upstream AudioServer mixed {frames} guest PCM frames at {mixer_tone:.0f} Hz", flush=True)
                    return
            if args.test == "presentation":
                rgb = memory[scan + (350 * WIDTH + 320) * 4:scan + (350 * WIDTH + 320) * 4 + 3]
                if not tapped and rgb == bytes.fromhex("5234c7"):
                    tap(320, 350)
                    print("Upstream page pixels verified; sending native touch", flush=True)
                    tapped = True
                elif tapped and rgb == bytes.fromhex("996c24"):
                    if args.screenshot:
                        screenshot(args.screenshot, memory[scan:scan+PIXELS])
                    print("Upstream compositor displayed the page and repainted it after native guest touch")
                    return
            if args.test == "youtube":
                media.observe_network(relay)
            if media and media.check(memory, scan, card, WIDTH, HEIGHT, tap):
                if args.screenshot:
                    screenshot(args.screenshot, memory[scan:scan+PIXELS])
                print(f"AArch64 media playback verified: {media}", flush=True)
                if not args.show_after_playback:
                    return
                window = open_viewer(args.show_after_playback)
                media = None
                args.test = None
                args.screenshot = None  # Keep the verified frame when the user closes the window.
            if args.test == "session" and browser_loaded and not home_tapped:
                pixels = memory[scan:scan+PIXELS]
                if pixels.count(bytes.fromhex("c4551a")) > 100:
                    if args.screenshot:
                        screenshot(args.screenshot, pixels)
                    # Ladybird's Home button, in the toolbar at x=95, y=63.
                    tap(95, 63)
                    home_tapped = True
            if args.test == "session" and home_restored:
                restored = memory[scan:scan+PIXELS:431]
                matches = sum(a == b for a, b in zip(home_pixels, restored))
                if matches > .85 * len(restored):
                    print("AArch64 Home opened Ladybird, showed the start page, and regained focus and pixels", flush=True)
                    return
            while reports and report_write(memory, input_offset, reports[0]):
                reports.popleft()
            if args.test in ("home", "ladybird") and tapped and activated:
                print(f"AArch64 {args.test} handled the touch", flush=True)
                return
        if guest.poll() is not None:
            raise SystemExit(f"AArch64 QEMU exited with status {guest.returncode}")
        if window and window.returncode:
            raise SystemExit(f"VioletOS SDL viewer exited with status {window.returncode}")
        if args.screenshot:
            screenshot(args.screenshot, memory[scan:scan+PIXELS])


if __name__ == "__main__":
    main()
