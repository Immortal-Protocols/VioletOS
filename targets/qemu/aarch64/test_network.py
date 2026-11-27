"""Run the AArch64 network_test.rs ELF on a network-enabled VioletOS image."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
from threading import Thread


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    requests = []
    with socket.create_server(("127.0.0.1", 0)) as server:
        server.settimeout(35)

        def serve():
            with server.accept()[0] as client:
                client.settimeout(10)
                with client.makefile("rb") as request:
                    requests.append(request.read(128))
                client.sendall(b"Violet socket reply\n")

        worker = Thread(target=serve, daemon=True)
        worker.start()
        host = f"127.0.0.1:{server.getsockname()[1]}"
        env = dict(os.environ, VIOLET_NET_HOSTS=json.dumps({"network-test.violet": host}))
        subprocess.run([sys.executable, Path(__file__).with_name("viewer.py"), "--out", args.out,
                        "--expect", "AArch64 VioletOS network test passed", "--timeout", "30"],
                       check=True, env=env)
        worker.join(1)
        if requests != [b"Violet socket probe\n"]:
            raise AssertionError("server did not receive the guest request and EOF")


if __name__ == "__main__":
    main()
