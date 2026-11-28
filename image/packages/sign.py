"""Publisher signatures for VPK2 packages (ECDSA P-256 over SHA-256).

A VPK2 record is  header[24] | image[size] | trailer[80]:

    trailer = "VSG1" | key_id u32 | version u32 | target u32 | r[32] | s[32]

The signature covers every byte before it: the header (ABI, component ID,
code and image lengths, CRC), the image, and the trailer's key ID, security
version and target tag. The supervisor (system/package-manager/verify.rs)
checks the same bytes with BearSSL against the keys pinned in the boot image.

Keys
- Developer builds (the default) sign with a key generated once per build
  directory, build/sel4/<target>/developer-signing.pem (key ID 2). It never
  leaves the machine and is not a production key.
- VIOLET_RELEASE must be unset, 0 or 1. Any other value stops the build, and so
  does a signing key or command given without VIOLET_RELEASE=1: neither may
  quietly produce a developer-keyed image.
- Release builds (VIOLET_RELEASE=1) pin only the release root public key
  (key ID 1, image/keys/release-root.pub.pem or VIOLET_RELEASE_PUBLIC_KEY) and
  sign with VIOLET_SIGNING_KEY (a PEM private key) or VIOLET_SIGN_COMMAND (a
  command that reads the bytes to sign on stdin and writes a DER ECDSA-SHA256
  signature to stdout, for a key held in an HSM). The build stops if the
  signatures it gets do not verify under the pinned release key.
"""

import hashlib
import json
import os
from pathlib import Path
import shlex
import struct
import subprocess
import sys
import tempfile

HEADER = 24
SIGNED_TRAILER = 16
TRAILER = SIGNED_TRAILER + 64
RELEASE_KEY_ID = 1
DEVELOPER_KEY_ID = 2
TARGETS = {"qemu": b"QEMU", "moto": b"MOTO"}
SPKI = bytes.fromhex("3059301306072a8648ce3d020106082a8648ce3d030107034200")
ORDER = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551


def der_to_raw(der):
    """ASN.1 SEQUENCE { INTEGER r, INTEGER s } to r[32] | s[32]."""

    def integer(pos):
        if pos + 2 > len(der) or der[pos] != 2 or der[pos + 1] & 0x80:
            raise ValueError("invalid ECDSA signature")
        end = pos + 2 + der[pos + 1]
        value = int.from_bytes(der[pos + 2 : end], "big")
        if end > len(der) or not 0 < value < ORDER:
            raise ValueError("invalid ECDSA signature")
        return value.to_bytes(32, "big"), end

    if len(der) < 8 or der[0] != 0x30 or der[1] & 0x80 or der[1] != len(der) - 2:
        raise ValueError("invalid ECDSA signature")
    r, pos = integer(2)
    s, pos = integer(pos)
    if pos != len(der):
        raise ValueError("invalid ECDSA signature")
    return r + s


def raw_to_der(raw):
    def integer(value):
        body = value.lstrip(b"\0") or b"\0"
        if body[0] & 0x80:
            body = b"\0" + body
        return bytes([2, len(body)]) + body

    body = integer(raw[:32]) + integer(raw[32:])
    return bytes([0x30, len(body)]) + body


def public_point(pem):
    """The 65-byte uncompressed point of a P-256 key in a PEM file (private or public)."""
    text = Path(pem).read_text()
    command = ["openssl", "ec", "-in", str(pem), "-pubout", "-outform", "DER", "-conv_form", "uncompressed"]
    if "PRIVATE KEY" not in text:
        command.insert(2, "-pubin")
    der = subprocess.run(command, capture_output=True, check=True).stdout
    if len(der) != len(SPKI) + 65 or not der.startswith(SPKI + b"\x04"):
        raise SystemExit(f"{pem} is not a P-256 key")
    return der[len(SPKI) :]


def openssl_verify(point, message, raw):
    """True when raw (r|s) is a valid ECDSA-SHA256 signature of message by point."""
    with tempfile.TemporaryDirectory() as directory:
        d = Path(directory)
        (d / "key.der").write_bytes(SPKI + point)
        (d / "sig").write_bytes(raw_to_der(raw))
        (d / "message").write_bytes(message)
        return (
            subprocess.run(
                ["openssl", "dgst", "-sha256", "-verify", d / "key.der", "-keyform", "DER",
                 "-signature", d / "sig", d / "message"],
                capture_output=True,
            ).returncode
            == 0
        )


class Signer:
    """One signing identity: who signs (key ID), for which target, and how."""

    def __init__(self, target, key_id, keys, private=None, command=None, release=False):
        self.target, self.key_id, self.keys = TARGETS[target], key_id, dict(keys)
        self.private, self.command, self.release = private, command, release

    def signature(self, message):
        if self.command:
            der = subprocess.run(shlex.split(self.command), input=message, capture_output=True, check=True).stdout
        else:
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "message"
                path.write_bytes(message)
                der = subprocess.run(
                    ["openssl", "dgst", "-sha256", "-sign", self.private, path], capture_output=True, check=True
                ).stdout
        raw = der_to_raw(der)
        # Never ship a package the pinned key would refuse.
        if not openssl_verify(self.keys[self.key_id], message, raw):
            raise SystemExit("The package signature does not verify under the pinned key (wrong signing key?)")
        return raw

    def sign(self, unsigned, version):
        """unsigned is header[24] | image; returns the complete VPK2 record."""
        if not 0 < version < 1 << 32:
            raise ValueError("a package security version is 1 or more")
        if len(unsigned) < HEADER or unsigned[:4] != b"VPK2" or len(unsigned) != HEADER + struct.unpack_from("<I", unsigned, 16)[0]:
            raise ValueError("not an unsigned VPK2 header and image")
        message = bytes(unsigned) + b"VSG1" + struct.pack("<2I", self.key_id, version) + self.target
        return message + self.signature(message)


def fields(blob):
    """(key_id, version, target) of a complete record, after checking the trailer's shape."""
    if len(blob) < HEADER + TRAILER:
        raise ValueError("truncated package")
    size = struct.unpack_from("<I", blob, 16)[0]
    if blob[:4] != b"VPK2" or len(blob) != HEADER + size + TRAILER:
        raise ValueError("package has no signature trailer")
    magic, key_id, version, target = struct.unpack_from("<4s2I4s", blob, HEADER + size)
    if magic != b"VSG1" or not version:
        raise ValueError("invalid signature trailer")
    return key_id, version, target


def verify(blob, keys, target):
    """Check a record's signature against pinned keys {key_id: point}; returns its version."""
    key_id, version, tag = fields(blob)
    if tag != TARGETS[target]:
        raise ValueError("package was signed for another target")
    if key_id not in keys:
        raise ValueError("package is signed by a key this image does not trust")
    if not openssl_verify(keys[key_id], bytes(blob[:-64]), bytes(blob[-64:])):
        raise ValueError("package signature is invalid")
    return version


def resign(blob, signer, version=None):
    """Sign an edited record again (tests that patch an image): fixes the CRC too."""
    import binascii

    size = struct.unpack_from("<I", blob, 16)[0]
    body = bytearray(blob[: HEADER + size])
    struct.pack_into("<I", body, 20, binascii.crc32(body[HEADER:]))
    return signer.sign(bytes(body), version or fields(blob)[1])


def developer_key(directory):
    key = Path(directory) / "developer-signing.pem"
    if not key.exists():
        key.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=key.parent) as temporary:
            new = Path(temporary) / "key.pem"
            subprocess.run(["openssl", "ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", new], check=True)
            new.chmod(0o600)
            new.replace(key)
    return key


def config(repo, target, environ=None):
    """The signer and pinned keys of this build. Fails closed for a release without keys."""
    environ = os.environ if environ is None else environ
    release_public = Path(environ.get("VIOLET_RELEASE_PUBLIC_KEY", repo / "image/keys/release-root.pub.pem"))
    flavour = environ.get("VIOLET_RELEASE", "")
    # Anything but an exact 1 must not quietly become a developer-keyed image.
    if flavour not in ("", "0", "1"):
        raise SystemExit(f"VIOLET_RELEASE={flavour}: use VIOLET_RELEASE=1 for a release build, or leave it unset (or 0) for a developer build")
    if flavour != "1":
        for name in ("VIOLET_SIGNING_KEY", "VIOLET_SIGN_COMMAND"):
            if environ.get(name):
                raise SystemExit(f"{name} is set without VIOLET_RELEASE=1: a developer build would ignore it and sign with the developer key")
    if flavour == "1":
        if not release_public.is_file():
            raise SystemExit(f"VIOLET_RELEASE=1 needs the release root public key at {release_public}")
        private, command = environ.get("VIOLET_SIGNING_KEY"), environ.get("VIOLET_SIGN_COMMAND")
        if not private and not command:
            raise SystemExit("VIOLET_RELEASE=1 needs VIOLET_SIGNING_KEY or VIOLET_SIGN_COMMAND")
        if private and not Path(private).is_file():
            raise SystemExit(f"VIOLET_SIGNING_KEY: {private} not found")
        # Only the release root is pinned: no developer key in a release image.
        return Signer(target, RELEASE_KEY_ID, {RELEASE_KEY_ID: public_point(release_public)},
                      private=private, command=None if private else command, release=True)
    key = developer_key(Path(environ.get("VIOLET_DEVELOPER_KEY_DIR", repo / "build/sel4" / target)))
    keys = {DEVELOPER_KEY_ID: public_point(key)}
    if release_public.is_file():
        keys[RELEASE_KEY_ID] = public_point(release_public)
    return Signer(target, DEVELOPER_KEY_ID, keys, private=key)


def version_overrides(signer, environ=None):
    """{component name: version} from VIOLET_PACKAGE_VERSIONS="radio=2,audio=3".

    For local tests of the version floors (system/package-manager/
    test_signed_boot.py), which need a factory catalog above version 1. A
    release build refuses it: release versions come from components.json alone.
    """
    text = (os.environ if environ is None else environ).get("VIOLET_PACKAGE_VERSIONS", "")
    if not text:
        return {}
    if signer.release:
        raise SystemExit("VIOLET_PACKAGE_VERSIONS is for developer test builds, not VIOLET_RELEASE=1")
    versions = {}
    for item in text.split(","):
        name, _, value = item.partition("=")
        if not name or not value.isdigit() or not 0 < int(value) < 1 << 32:
            raise SystemExit(f"VIOLET_PACKAGE_VERSIONS: expected name=version, got {item!r}")
        versions[name] = int(value)
    return versions


def summary(signer):
    """One line for the end of an image build: which keys the image trusts."""
    names = {RELEASE_KEY_ID: "release root", DEVELOPER_KEY_ID: "developer"}
    pinned = ", ".join(f"{names.get(k, 'key')} (ID {k})" for k in sorted(signer.keys))
    if signer.release:
        return f"Package signing: RELEASE image. Trusts only: {pinned}."
    return (f"Package signing: DEVELOPER image, not for release. Trusts: {pinned}; "
            f"signed with the developer key (ID {signer.key_id}).")


def write_keys(signer, out):
    """The pinned keys for the supervisor (Rust) and for the host tools (JSON)."""
    target = struct.unpack("<I", signer.target)[0]
    rows = "".join(f"    ({key_id}, [{','.join(map(str, point))}]),\n" for key_id, point in sorted(signer.keys.items()))
    rust = Path(out) / "package_keys.rs"
    rust.write_text(
        "// Generated by image/packages/sign.py: the package signing keys this image trusts.\n"
        f"pub const TARGET: u32 = {target:#x};\n"
        f"pub const RELEASE: bool = {'true' if signer.release else 'false'};\n"
        f"pub const KEYS: &[(u32, [u8; 65])] = &[\n{rows}];\n"
    )
    (Path(out) / "package-keys.json").write_text(
        json.dumps({"target": signer.target.decode(), "release": signer.release,
                    "keys": {str(k): v.hex() for k, v in sorted(signer.keys.items())}}, indent=1) + "\n"
    )
    return rust


def load_keys(path):
    """(target name, {key_id: point}) from a build's package-keys.json."""
    data = json.loads(Path(path).read_text())
    target = next(name for name, tag in TARGETS.items() if tag.decode() == data["target"])
    return target, {int(k): bytes.fromhex(v) for k, v in data["keys"].items()}


def digest(blob):
    return hashlib.sha256(blob[:-64]).hexdigest()


if __name__ == "__main__":
    # sign.py verify <package-keys.json> <file.vpk>...
    if len(sys.argv) < 4 or sys.argv[1] != "verify":
        sys.exit("usage: sign.py verify build/sel4/<target>/package-keys.json FILE.vpk...")
    target_name, pinned = load_keys(sys.argv[2])
    for name in sys.argv[3:]:
        try:
            data = Path(name).read_bytes()
            print(name, "key", fields(data)[0], "version", verify(data, pinned, target_name))
        except (OSError, ValueError) as exc:
            sys.exit(f"{name}: {exc}")
