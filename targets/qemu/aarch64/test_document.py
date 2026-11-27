"""Exercise inline DOM JavaScript through VioletOS Ladybird's normal HTTP loader.

Boot a graphical browser image with --arg=http://document.test/inline.html.
"""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import sys
from threading import Thread


class Page(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/inline.html":
            self.send_error(404)
            return
        body = b"""<!doctype html><title>Waiting</title><h1 id="answer"></h1>
<script>
document.querySelector('#answer').textContent = [2,3,7].reduce((a,b)=>a*b,1);
document.title = document.location.hostname + ' result ' + document.querySelector('h1').textContent;
</script>"""
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    with ThreadingHTTPServer(("127.0.0.1", 0), Page) as server:
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            host = f"127.0.0.1:{server.server_port}"
            env = dict(os.environ, VIOLET_NET_HOSTS=json.dumps({"document.test": host}))
            subprocess.run([sys.executable, Path(__file__).with_name("viewer.py"), "--out", args.out,
                            "--expect", 'title="document.test result 42"', "--timeout", "45"], check=True, env=env)
        finally:
            server.shutdown()
            thread.join()


if __name__ == "__main__":
    main()
