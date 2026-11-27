#!/usr/bin/env python3
"""Radio's TLS client (tls.c) against a TLS server on this Mac's loopback.

tls.c links only the algorithms of its two cipher suites, so nothing else
shows whether a handshake still completes. This builds tls.c for the host with
BearSSL's source (build/sel4/bearssl-0.6, there after an image build) and a
trust anchor made here in place of ISRG Root X1, then fetches the UUID page
from a local server. Local only (`make test-radio-tls`), not in CI.
"""
import os
import shutil
import socket
import ssl
import subprocess
import sys
import threading
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BEARSSL = ROOT / "build/sel4/bearssl-0.6"
OUT = ROOT / "build/tests/radio-tls"
HOST = "www.uuidgenerator.net"
UUID = "3f2b8c1e-9a4d-4c7b-8e21-0d5f6a7b8c9d"

HARNESS = r"""
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>
static int fd;
int radio_entropy(unsigned char seed[32]) { arc4random_buf(seed, 32); return 1; }
unsigned radio_unix_time(void) { return (unsigned)time(0); }
int tunnel_read(void *c, unsigned char *b, size_t n) { ssize_t r = read(fd, b, n); return r > 0 ? (int)r : -1; }
int tunnel_write(void *c, const unsigned char *b, size_t n) { ssize_t r = write(fd, b, n); return r > 0 ? (int)r : -1; }
int guest_https(unsigned char uuid[36]);
int main(int argc, char **argv) {
  struct sockaddr_in a = {.sin_family = AF_INET, .sin_port = htons(atoi(argv[1]))};
  inet_pton(AF_INET, "127.0.0.1", &a.sin_addr);
  fd = socket(AF_INET, SOCK_STREAM, 0);
  if (connect(fd, (struct sockaddr *)&a, sizeof a)) return 3;
  unsigned char uuid[37] = {0};
  if (!guest_https(uuid)) return 1;
  puts((char *)uuid);
  return 0;
}
"""


def sh(*args, **kw):
    return subprocess.run([str(a) for a in args], check=True, capture_output=True, **kw)


def tlv(data, at):
    """(tag, start of the value, end) of the DER item at `at`."""
    length, pos = data[at + 1], at + 2
    if length & 0x80:
        count = length & 0x7F
        length, pos = int.from_bytes(data[pos:pos + count], "big"), pos + count
    return data[at], pos, pos + length


def anchor(cert_pem, directory):
    """A header like isrg_x1.h for a self-signed RSA root made here."""
    der = sh("openssl", "x509", "-in", cert_pem, "-outform", "DER").stdout
    _, tbs, _ = tlv(der, tlv(der, 0)[1])
    items, at = [], tbs
    if der[tbs] != 0xA0:  # a version 1 certificate has no version field
        items.append(None)
    while len(items) < 7:
        tag, start, end = tlv(der, at)
        items.append((at, start, end))
        at = end
    subject = der[items[5][0]:items[5][2]]          # version, serial, algorithm, issuer, validity, subject, key
    _, bits, _ = tlv(der, tlv(der, items[6][1])[2])  # the BIT STRING after the algorithm
    _, key, _ = tlv(der, bits + 1)
    _, n0, n1 = tlv(der, key)
    _, e0, e1 = tlv(der, n1)
    n, e = der[n0:n1].lstrip(b"\0"), der[e0:e1]

    def array(name, value):
        return f"static const unsigned char {name}[] = {{{','.join(map(str, value))}}};\n"

    (directory / "isrg_x1.h").write_text(
        "#pragma once\n#include <bearssl.h>\n" + array("TA0_DN", subject) + array("TA0_RSA_N", n) + array("TA0_RSA_E", e)
        + "static const br_x509_trust_anchor TAs[1] = {{{(unsigned char *)TA0_DN, sizeof TA0_DN}, BR_X509_TA_CA,"
        " {BR_KEYTYPE_RSA, {.rsa = {(unsigned char *)TA0_RSA_N, sizeof TA0_RSA_N, (unsigned char *)TA0_RSA_E, sizeof TA0_RSA_E}}}}};\n"
        "#define TAs_NUM 1\n")


def certificates(d):
    """An RSA root; under it an RSA leaf, and a P-384 CA with a P-256 leaf
    signed with SHA-384 (the shape of the real site's chain); and a stranger."""
    ca_ext = d / "ca.ext"
    ca_ext.write_text("basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign\n")
    leaf_ext = d / "leaf.ext"
    leaf_ext.write_text(f"basicConstraints=CA:FALSE\nsubjectAltName=DNS:{HOST}\n")
    other_ext = d / "other.ext"
    other_ext.write_text("basicConstraints=CA:FALSE\nsubjectAltName=DNS:elsewhere.test\n")

    def key(name, curve=None):
        if curve:
            sh("openssl", "ecparam", "-name", curve, "-genkey", "-noout", "-out", d / f"{name}.key")
        else:
            sh("openssl", "genrsa", "-out", d / f"{name}.key", "2048")

    def signed(name, by, ext, digest="-sha256"):
        sh("openssl", "req", "-new", "-key", d / f"{name}.key", "-subj", f"/CN={name}", "-out", d / f"{name}.csr")
        sh("openssl", "x509", "-req", "-in", d / f"{name}.csr", "-CA", d / f"{by}.pem", "-CAkey", d / f"{by}.key",
           "-CAcreateserial", "-days", "2", digest, "-extfile", ext, "-out", d / f"{name}.pem")

    for root in ("root", "stranger"):
        key(root)
        sh("openssl", "req", "-new", "-x509", "-key", d / f"{root}.key", "-subj", f"/O=Violet test/CN={root}", "-days", "2",
           "-sha256", "-out", d / f"{root}.pem")
    key("between", "secp384r1"); signed("between", "root", ca_ext)
    key("ec", "prime256v1"); signed("ec", "between", leaf_ext, "-sha384")
    key("rsa"); signed("rsa", "root", leaf_ext)
    key("wrongname"); signed("wrongname", "root", other_ext)
    key("unknown"); signed("unknown", "stranger", leaf_ext)
    (d / "ec-chain.pem").write_text((d / "ec.pem").read_text() + (d / "between.pem").read_text())


def client(d):
    """tls.c as it is in the tree, beside the test's trust anchor."""
    shutil.copy(ROOT / "services/radio/tls.c", d / "tls.c")
    (d / "harness.c").write_text(HARNESS)
    objects = d / "bearssl"
    objects.mkdir(exist_ok=True)
    flags = ["cc", "-O1", "-w", "-I", BEARSSL / "inc", "-I", BEARSSL / "src"]

    def compile(source):
        target = objects / (source.stem + ".o")
        if not target.exists():
            sh(*flags, "-c", source, "-o", target)
        return target

    with ThreadPoolExecutor(8) as pool:
        built = list(pool.map(compile, sorted((BEARSSL / "src").rglob("*.c"))))
    archive = d / "libbearssl.a"
    if not archive.exists():
        sh("ar", "rcs", archive, *built)
    sh(*flags, d / "tls.c", d / "harness.c", archive, "-o", d / "client")
    return d / "client"


def fetch(program, d, certificate, key, ciphers, body=UUID):
    """Serve one connection with `ciphers` and return the client's (exit code, output)."""
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.options |= getattr(ssl, "OP_NO_TLSv1_3", 0)
    context.set_ciphers(ciphers)
    context.load_cert_chain(d / certificate, d / key)
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    listener.settimeout(10)

    def serve():
        try:
            raw, _ = listener.accept()
            raw.settimeout(10)
            with context.wrap_socket(raw, server_side=True) as peer:
                request = b""
                while b"\r\n\r\n" not in request:
                    piece = peer.recv(4096)
                    if not piece:
                        return
                    request += piece
                if not request.startswith(b"GET /api/version4 HTTP/1.1\r\n"):
                    return
                peer.sendall(f"HTTP/1.1 200 OK\r\nContent-Length: {len(body)}\r\nConnection: close\r\n\r\n{body}".encode())
        except (OSError, ssl.SSLError):
            pass

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    result = subprocess.run([str(program), str(listener.getsockname()[1])], capture_output=True, text=True, timeout=20)
    thread.join(10)
    listener.close()
    return result.returncode, result.stdout.strip()


def main():
    if not (BEARSSL / "src").is_dir():
        sys.exit("BearSSL's source is missing: run make qemu-image (or make setup) first")
    OUT.mkdir(parents=True, exist_ok=True)
    certificates(OUT)
    anchor(OUT / "root.pem", OUT)
    program = client(OUT)
    ok = (0, UUID)
    cases = [
        ("ECDHE-ECDSA with AES-128-GCM, a P-256 leaf under a P-384 CA", "ec-chain.pem", "ec.key", "ECDHE-ECDSA-AES128-GCM-SHA256", True),
        ("ECDHE-RSA with AES-128-GCM", "rsa.pem", "rsa.key", "ECDHE-RSA-AES128-GCM-SHA256", True),
        ("a server that also offers other suites", "rsa.pem", "rsa.key",
         "ECDHE-RSA-CHACHA20-POLY1305:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-RSA-AES128-GCM-SHA256:AES128-SHA", True),
        ("a server with only ChaCha20-Poly1305", "rsa.pem", "rsa.key", "ECDHE-RSA-CHACHA20-POLY1305", False),
        ("a server with only AES-256-GCM", "rsa.pem", "rsa.key", "ECDHE-RSA-AES256-GCM-SHA384", False),
        ("a server with only CBC suites", "rsa.pem", "rsa.key", "ECDHE-RSA-AES128-SHA256:ECDHE-RSA-AES128-SHA:AES128-SHA", False),
        ("a server with only RSA key exchange", "rsa.pem", "rsa.key", "AES128-GCM-SHA256", False),
        ("a certificate for another name", "wrongname.pem", "wrongname.key", "ECDHE-RSA-AES128-GCM-SHA256", False),
        ("a certificate from an unknown root", "unknown.pem", "unknown.key", "ECDHE-RSA-AES128-GCM-SHA256", False),
    ]
    failed = False
    for name, certificate, key, ciphers, good in cases:
        got = fetch(program, OUT, certificate, key, ciphers)
        right = got == ok if good else got[0] == 1 and not got[1]
        failed |= not right
        print(f"{'ok  ' if right else 'FAIL'} {'fetched' if good else 'refused'}: {name}" + ("" if right else f" (got {got})"))
    got = fetch(program, OUT, "rsa.pem", "rsa.key", "ECDHE-RSA-AES128-GCM-SHA256", body="not a uuid")
    if got != (1, ""):
        failed = True
        print(f"FAIL refused: a page that is not a UUID (got {got})")
    if failed:
        sys.exit("Radio's TLS client: FAILED")
    print("Radio's TLS client: both suites fetch the page; other suites, names and roots are refused")


if __name__ == "__main__":
    main()
