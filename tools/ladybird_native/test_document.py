"""Boot native LibWeb, RequestServer and ImageDecoder in VioletOS and check script and image loading."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import struct
import zlib
from tempfile import TemporaryDirectory
from threading import Thread

ROOT = Path(__file__).resolve().parents[2]
TARGET = ROOT / 'targets/qemu/aarch64'


def test(overlay, out):
    requests = []
    def png_chunk(name, data):
        return struct.pack('>I', len(data)) + name + data + struct.pack('>I', zlib.crc32(name + data))
    image = (b'\x89PNG\r\n\x1a\n' + png_chunk(b'IHDR', struct.pack('>IIBBBBB', 2, 3, 8, 2, 0, 0, 0))
             + png_chunk(b'IDAT', zlib.compress((b'\x00' + b'\xff\x00\x00' * 2) * 3)) + png_chunk(b'IEND', b''))
    script = b'''window.addEventListener("load", () => {
        const img = document.querySelector("img");
        document.querySelector("h1").textContent = document.URL + " " + [2,3,7].reduce((a,b)=>a*b) +
            " image " + img.naturalWidth + "x" + img.naturalHeight;
    });'''
    responses = {'/script.js': ('text/javascript', script), '/image.png': ('image/png', image)}

    class Site(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path not in responses:
                self.send_error(404)
                return
            requests.append(self.path)
            mime, body = responses[self.path]
            self.send_response(200)
            self.send_header('Content-Type', mime)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    out.mkdir(parents=True, exist_ok=True)
    for app in ['usr/libexec/ladybird-dom', 'opt/ladybird/libexec/RequestServer', 'opt/ladybird/libexec/ImageDecoder']:
        if not (overlay / app).is_file():
            raise SystemExit(f'Build the native Ladybird overlay first. Missing {overlay / app}')
    with TemporaryDirectory(dir=out) as temp, ThreadingHTTPServer(('127.0.0.1', 0), Site) as server:
        root = Path(temp) / 'root'
        shutil.copytree(overlay, root)
        (root / 'document.html').write_text('<!doctype html><h1>Waiting</h1><img src="/image.png"><script src="/script.js"></script>')
        address = 'http://document.test/example'
        subprocess.run([sys.executable, TARGET / 'build.py', '--out', out, '--root', root,
                        '--elf', root / 'usr/libexec/ladybird-dom', '--network', '--arg=--html',
                        '--arg=--url', '--arg=' + address, '--arg=/document.html'], check=True)
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            env = dict(os.environ, VIOLET_NET_HOSTS=json.dumps({'document.test': f'127.0.0.1:{server.server_port}'}))
            subprocess.run([sys.executable, TARGET / 'viewer.py', '--out', out, '--timeout', '60',
                            '--expect', '<h1>' + address + ' 42 image 2x3</h1>'], check=True, env=env)
        finally:
            server.shutdown()
            thread.join()
        if set(requests) != set(responses):
            raise AssertionError(f'Unexpected script requests: {requests}')
        (root / 'frame.html').write_text('<iframe src="https://frame.example/child"></iframe>')
        subprocess.run([sys.executable, TARGET / 'build.py', '--out', out, '--root', root,
                        '--elf', root / 'usr/libexec/ladybird-dom', '--arg=--html',
                        '--arg=--url', '--arg=' + address, '--arg=/frame.html', '--test',
                        '--expect-exit', '1', '--expect', 'Document navigation requires the full Ladybird browser'], check=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT / 'build/ladybird-native/document/root')
    parser.add_argument('--out', type=Path, default=ROOT / 'build/sel4/qemu-aarch64-document')
    options = parser.parse_args()
    test(options.root.resolve(), options.out.resolve())
