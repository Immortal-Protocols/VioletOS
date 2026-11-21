"""Exercise the Moto WPA2 client against an independent WPA2-Personal authenticator."""
import ctypes as c
import hashlib
import hmac
import os
from pathlib import Path
import struct
import subprocess
import tempfile

from build_supplicant import FILES

HERE = Path(__file__).resolve().parent
SRC = HERE.parents[1] / "build/sel4/wpa_supplicant-2.11/src"
MAC = bytes.fromhex("020000000001")
AP = bytes.fromhex("020000000002")
NONCE = bytes(range(32))
PMK = hashlib.pbkdf2_hmac("sha1", b"test passphrase", b"fixture", 4096, 32)
RSN = bytes.fromhex("30140100000fac040100000fac040100000fac020000")
GTK = bytes(range(16))


def key_frame(info, replay, nonce, data=b"", kck=None):
    body = bytearray(struct.pack(">BHHQ", 2, info, 16, replay) + nonce +
                     bytes(48) + struct.pack(">H", len(data)) + data)
    packet = bytearray(struct.pack(">BBH", 2, 3, len(body)) + body)
    if kck:
        packet[81:97] = hmac.digest(kck, packet, "sha1")[:16]
    return bytes(packet)


def test(lib):
    assert lib.eloop_init() == 0
    lib.station_wpa_open.argtypes = [c.c_void_p, c.c_char_p, c.c_char_p, c.c_char_p, c.c_size_t,
                                     c.c_char_p, c.c_size_t, c.c_char_p, c.c_size_t, c.c_char_p]
    lib.station_wpa_rx.argtypes = [c.c_char_p, c.c_size_t]
    assert lib.station_wpa_open(1, MAC, AP, b"fixture", 7, RSN, len(RSN), RSN, len(RSN), PMK) == 0
    first = key_frame(0x008a, 1, NONCE)
    assert lib.station_wpa_rx(first, len(first)) == 0
    out = c.create_string_buffer(2048)
    n = lib.sent_frame(out)
    second = bytearray(out.raw[:n])
    assert second[5:7] == bytes.fromhex("010a") and second[99:] == RSN
    snonce = bytes(second[17:49])
    context = b"".join(sorted([MAC, AP]) + sorted([snonce, NONCE]))
    prefix = b"Pairwise key expansion\0" + context
    ptk = b"".join(hmac.digest(PMK, prefix + bytes([i]), "sha1") for i in range(3))
    kck, kek, tk = ptk[:16], ptk[16:32], ptk[32:48]
    mic, second[81:97] = bytes(second[81:97]), bytes(16)
    assert mic == hmac.digest(kck, second, "sha1")[:16]
    plain = RSN + bytes.fromhex("dd16000fac010100") + GTK + bytes.fromhex("dd00")
    # RFC 3394 with the host AES primitive, independent of the client under test.
    blocks = [plain[i:i+8] for i in range(0, len(plain), 8)]
    iv = bytes([0xa6])*8
    for cycle in range(6):
        for i, block in enumerate(blocks):
            enc = subprocess.run(["openssl", "enc", "-aes-128-ecb", "-nopad", "-K", kek.hex()],
                                 input=iv + block, capture_output=True, check=True).stdout
            t = cycle * len(blocks) + i + 1
            iv = (int.from_bytes(enc[:8], "big") ^ t).to_bytes(8, "big")
            blocks[i] = enc[8:]
    wrapped = iv + b"".join(blocks)
    third = key_frame(0x13ca, 2, NONCE, wrapped, kck)
    bad = bytearray(third)
    bad[81] ^= 1
    lib.station_wpa_rx(bytes(bad), len(bad))
    assert all(lib.observed_key(pair, out) == 0 for pair in (0, 1))
    assert lib.station_wpa_rx(third, len(third)) == 1
    for pair, key in [(0, GTK), (1, tk)]:
        assert lib.observed_key(pair, out) == 1 and out.raw[:16] == key
    n = lib.sent_frame(out)
    fourth = bytearray(out.raw[:n])
    assert fourth[5:7] == bytes.fromhex("030a") and fourth[9:17] == third[9:17]
    mic, fourth[81:97] = bytes(fourth[81:97]), bytes(16)
    assert mic == hmac.digest(kck, fourth, "sha1")[:16]
    lib.station_wpa_rx(third, len(third))
    assert all(lib.observed_key(pair, out) == 1 for pair in (0, 1))
    lib.station_wpa_close()
    lib.eloop_destroy()


if __name__ == "__main__":
    with tempfile.TemporaryDirectory() as tmp:
        so = Path(tmp) / "station.so"
        sources = [SRC / (name + ".c") for name in FILES + ["utils/os_unix", "utils/eloop", "utils/wpa_debug"]]
        subprocess.run([os.environ.get("CC", "cc"), "-shared", "-fPIC", "-O1", "-I", SRC,
                        "-I", SRC / "utils", "-DCONFIG_NO_STDOUT_DEBUG", "-DCONFIG_NO_RANDOM_POOL",
                        "-DCONFIG_CRYPTO_INTERNAL", "-DCONFIG_NO_RC4", "-DCONFIG_INTERNAL_AES", "-DCONFIG_INTERNAL_SHA1", "-DCONFIG_INTERNAL_SHA256",
                        "-DCONFIG_INTERNAL_MD5", "-DCONFIG_NO_WPA_MSG", "-DCONFIG_NO_WPA_TRACE",
                        *sources, HERE / "supplicant.c", HERE / "test_supplicant.c", "-o", so], check=True)
        test(c.CDLL(str(so)))
        print("Moto WPA2 four-way exchange, invalid MIC, key contents, and replay checks passed")
