"""Exercise the Moto WPA2 client against an independent WPA2-Personal authenticator.

The authenticator here is written from IEEE 802.11 with Python's hashlib/hmac
and the host's AES (openssl), so it shares no code with the client under test.
It covers PSK and PSK-SHA256 key management, with and without management frame
protection (802.11w: the IGTK in message 3). It also recomputes the two
protected-frame vectors that pmf.rs's host tests embed, with hostap's own
AES-CMAC and AES-CCM, and runs test_pmf.rs on hostap's AES (the handset's
cipher) instead of the test-only one.

Needs hostap's sources: build/sel4/wpa_supplicant-2.11 (fetched by `make moto`
or `make test-wpa`), or VIOLET_HOSTAP_SRC pointing at its src directory.
"""
import ctypes as c
import hashlib
import hmac
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile

from build_supplicant import FILES

HERE = Path(__file__).resolve().parent
SRC = Path(os.environ.get("VIOLET_HOSTAP_SRC", HERE.parents[1] / "build/sel4/wpa_supplicant-2.11/src"))
MAC = bytes.fromhex("020000000001")
AP = bytes.fromhex("020000000002")
NONCE = bytes(range(32))
PMK = hashlib.pbkdf2_hmac("sha1", b"test passphrase", b"fixture", 4096, 32)
GTK = bytes(range(16))
IGTK = bytes(range(0x40, 0x50))
IPN = bytes.fromhex("0a0b0c000000")


def rsne(akm, caps, group_management=False):
    """An RSN element: CCMP-128 group and pairwise, one AKM, capabilities."""
    body = bytes.fromhex("0100000fac040100000fac040100000fac") + bytes([akm]) + struct.pack("<H", caps)
    if group_management:
        body += bytes.fromhex("0000000fac06")  # no PMKIDs, BIP-CMAC-128
    return bytes([48, len(body)]) + body


def aes(key, block):
    return subprocess.run(["openssl", "enc", "-aes-128-ecb", "-nopad", "-K", key.hex()],
                          input=block, capture_output=True, check=True).stdout


def cmac(key, message):
    """AES-CMAC (RFC 4493)."""
    def double(block):
        n = int.from_bytes(block, "big") << 1
        return ((n & ((1 << 128) - 1)) ^ (0x87 if n >> 128 else 0)).to_bytes(16, "big")
    k1 = double(aes(key, bytes(16)))
    blocks = [message[i:i + 16] for i in range(0, len(message), 16)] or [b""]
    last = blocks.pop()
    if len(last) == 16:
        last = bytes(a ^ b for a, b in zip(last, k1))
    else:
        last = bytes(a ^ b for a, b in zip(last + b"\x80" + bytes(15 - len(last)), double(k1)))
    state = bytes(16)
    for block in blocks + [last]:
        state = aes(key, bytes(a ^ b for a, b in zip(state, block)))
    return state


def wrap(kek, plain):
    """RFC 3394 key wrap with the host AES primitive."""
    blocks = [plain[i:i + 8] for i in range(0, len(plain), 8)]
    iv = bytes([0xa6]) * 8
    for cycle in range(6):
        for i, block in enumerate(blocks):
            enc = aes(kek, iv + block)
            t = cycle * len(blocks) + i + 1
            iv = (int.from_bytes(enc[:8], "big") ^ t).to_bytes(8, "big")
            blocks[i] = enc[8:]
    return iv + b"".join(blocks)


def ptk(akm, snonce):
    context = b"".join(sorted([MAC, AP]) + sorted([snonce, NONCE]))
    label = b"Pairwise key expansion"
    if akm == 6:
        # IEEE 802.11 KDF-SHA-256-384: counter, label, context, length in bits.
        out = b"".join(hmac.digest(PMK, struct.pack("<H", i) + label + context + struct.pack("<H", 384), "sha256")
                       for i in (1, 2))
    else:
        out = b"".join(hmac.digest(PMK, label + b"\0" + context + bytes([i]), "sha1") for i in range(3))
    return out[:16], out[16:32], out[32:48]


def mic(akm, kck, packet):
    return cmac(kck, packet) if akm == 6 else hmac.digest(kck, packet, "sha1")[:16]


def key_frame(info, replay, nonce, data=b"", kck=None, akm=2):
    body = bytearray(struct.pack(">BHHQ", 2, info, 16, replay) + nonce +
                     bytes(48) + struct.pack(">H", len(data)) + data)
    packet = bytearray(struct.pack(">BBH", 2, 3, len(body)) + body)
    if kck:
        packet[81:97] = mic(akm, kck, bytes(packet))
    return bytes(packet)


def handshake(lib, akm, mfp):
    """The four-way exchange with key management `akm` (2 PSK, 6 PSK-SHA256) and
    management frame protection `mfp` (0 off, 1 optional, 2 required)."""
    version = 3 if akm == 6 else 2  # key descriptor: AES-128-CMAC or HMAC-SHA1
    caps = {0: 0, 1: 0x80, 2: 0xc0}[mfp]
    ap_rsne = rsne(akm, caps)
    ours = rsne(akm, caps, group_management=mfp != 0)
    lib.reset_observed()
    assert lib.eloop_init() == 0
    assert lib.station_wpa_open(1, MAC, AP, b"fixture", 7, ap_rsne, len(ap_rsne), ours, len(ours), PMK, akm, mfp) == 0
    first = key_frame(0x0088 | version, 1, NONCE)
    assert lib.station_wpa_rx(first, len(first)) == 0
    out = c.create_string_buffer(2048)
    n = lib.sent_frame(out)
    second = bytearray(out.raw[:n])
    assert second[5:7] == bytes([1, 8 | version]) and second[99:] == ours
    snonce = bytes(second[17:49])
    kck, kek, tk = ptk(akm, snonce)
    sent, second[81:97] = bytes(second[81:97]), bytes(16)
    assert sent == mic(akm, kck, bytes(second))
    plain = ap_rsne + bytes.fromhex("dd16000fac010100") + GTK
    if mfp:
        plain += bytes.fromhex("dd1c000fac090400") + IPN + IGTK
    plain += b"\xdd" + bytes(-(len(plain) + 1) % 8)
    third = key_frame(0x13c8 | version, 2, NONCE, wrap(kek, plain), kck, akm)
    bad = bytearray(third)
    bad[81] ^= 1
    lib.station_wpa_rx(bytes(bad), len(bad))
    assert all(lib.observed_key(pair, out) == 0 for pair in (0, 1))
    assert lib.observed_igtk(4, out) == 0
    assert lib.station_wpa_rx(third, len(third)) == 1
    for pair, key in [(0, GTK), (1, tk)]:
        assert lib.observed_key(pair, out) == 1 and out.raw[:16] == key
    if mfp:
        assert lib.observed_igtk(4, out) == 1 and out.raw[:22] == IGTK + IPN
    else:
        assert lib.observed_igtk(4, out) == 0
    assert lib.observed_igtk(5, out) == 0
    n = lib.sent_frame(out)
    fourth = bytearray(out.raw[:n])
    assert fourth[5:7] == bytes([3, 8 | version]) and fourth[9:17] == third[9:17]
    sent, fourth[81:97] = bytes(fourth[81:97]), bytes(16)
    assert sent == mic(akm, kck, bytes(fourth))
    lib.station_wpa_rx(third, len(third))
    assert all(lib.observed_key(pair, out) == 1 for pair in (0, 1))
    assert lib.observed_igtk(4, out) == (1 if mfp else 0)
    # The link is up. Key frames that anyone in range can send are rejected
    # (-2) without ending it (-1): Radio drops them and stays joined.
    wrapped = wrap(kek, plain)
    forged = {
        "message 1 replayed": first,
        "message 3 replayed": third,
        "wrong MIC, new counter": key_frame(0x13c8 | version, 9, NONCE, wrapped, bytes(16), akm),
        "no MIC, encrypted data": key_frame(0x12c8 | version, 9, NONCE, wrapped),
        "request bit": key_frame(0x0888 | version, 9, NONCE),
        "no ack bit": key_frame(0x0008 | version, 9, NONCE),
        "SMK bit": key_frame(0x2088 | version, 9, NONCE),
        "another descriptor version": key_frame(0x0088 | 1, 9, NONCE),
        "key data longer than the frame": key_frame(0x0088 | version, 9, NONCE)[:-2] + b"\xff\xff",
    }
    for name, frame in forged.items():
        assert lib.station_wpa_rx(frame, len(frame)) == -2, name
    # Frames hostap ignores or acts on without a verdict do not end it either:
    # a truncated one, another EAPOL type, and a message 1 with a new counter
    # (it has no MIC; hostap answers it and waits for a message 3 that the
    # forger cannot make).
    for frame in [first[:50], bytes([2, 0]) + first[2:], key_frame(0x0088 | version, 10, bytes(range(32, 64)))]:
        assert lib.station_wpa_rx(frame, len(frame)) in (0, 1)
    assert [lib.observed_key(pair, out) for pair in (0, 1)] == [1, 1]
    # The access point's next group rekey is still taken: the next GTK and,
    # with 802.11w, the IGTK with the other key ID.
    new_igtk = bytes(range(0x60, 0x70))
    plain = bytes.fromhex("dd16000fac010200") + bytes(range(16, 32))
    if mfp:
        plain += bytes.fromhex("dd1c000fac090500") + bytes(6) + new_igtk
    plain += b"\xdd" + bytes(-(len(plain) + 1) % 8)
    group = key_frame(0x1380 | version, 3, bytes(32), wrap(kek, plain), kck, akm)
    assert lib.station_wpa_rx(group, len(group)) != -1
    assert lib.observed_key(0, out) == 2 and out.raw[:16] == bytes(range(16, 32))
    if mfp:
        assert lib.observed_igtk(5, out) == 1 and out.raw[:22] == new_igtk + bytes(6)
        assert lib.observed_igtk(4, out) == 1
    lib.station_wpa_close()
    # Without a link every frame is refused.
    assert lib.station_wpa_rx(first, len(first)) == -1
    lib.eloop_destroy()


def refused(lib):
    """Parameters the glue must not accept."""
    plain = rsne(2, 0)
    assert lib.eloop_init() == 0
    for akm, mfp in [(1, 0), (8, 1), (2, 3), (6, -1)]:
        assert lib.station_wpa_open(1, MAC, AP, b"fixture", 7, plain, len(plain), plain, len(plain), PMK, akm, mfp) == -1
    lib.eloop_destroy()


def protected_frame_vectors(lib):
    """The vectors in pmf.rs, from hostap's AES-CMAC and AES-CCM."""
    # The inputs of the IEEE 802.11 BIP example (a broadcast Deauthentication frame).
    igtk = bytes.fromhex("4ea9543e09cf2b1eca66ffc58bdecbcf")
    header = bytes.fromhex("c0000000ffffffffffff0200000000000200000000000900")
    body = bytes.fromhex("0200") + bytes.fromhex("4c1004000400000000000000000000000000")
    covered = header[:2] + header[4:22] + body
    out = c.create_string_buffer(16)
    assert lib.omac1_aes_128(igtk, covered, len(covered), out) == 0
    assert out.raw == cmac(igtk, covered)
    assert out.raw[:8].hex() == BIP_MIC, out.raw[:8].hex()
    # An SA Query response from 02:00:00:00:01:00 to 02:00:00:00:00:00, PN 1.
    tk = bytes.fromhex("66ed21042f9f26d7115706e40414cf2e")
    header = bytes.fromhex("d0400000" "020000000000" "020000000100" "020000000000" "0000")
    aad = header[:2] + header[4:22] + bytes(2)
    nonce = b"\x10" + header[10:16] + (1).to_bytes(6, "big")
    plain = bytes.fromhex("08013412")
    crypt, tag = c.create_string_buffer(4), c.create_string_buffer(8)
    assert lib.aes_ccm_ae(tk, 16, nonce, 8, plain, 4, aad, len(aad), crypt, tag) == 0
    assert (crypt.raw + tag.raw).hex() == SA_QUERY, (crypt.raw + tag.raw).hex()


# The same constants as in pmf.rs (bip_matches..., protected_sa_query_matches...).
BIP_MIC = "48dfbfa7b8278872"
SA_QUERY = "c9d7367a93b7955b0c12e982"


def test(lib):
    lib.station_wpa_open.argtypes = [c.c_void_p, c.c_char_p, c.c_char_p, c.c_char_p, c.c_size_t,
                                     c.c_char_p, c.c_size_t, c.c_char_p, c.c_size_t, c.c_char_p,
                                     c.c_int, c.c_int]
    lib.station_wpa_rx.argtypes = [c.c_char_p, c.c_size_t]
    lib.omac1_aes_128.argtypes = [c.c_char_p, c.c_char_p, c.c_size_t, c.c_char_p]
    lib.aes_ccm_ae.argtypes = [c.c_char_p, c.c_size_t, c.c_char_p, c.c_size_t, c.c_char_p, c.c_size_t,
                               c.c_char_p, c.c_size_t, c.c_char_p, c.c_char_p]
    for akm, mfp in [(2, 0), (2, 1), (2, 2), (6, 0), (6, 1), (6, 2)]:
        handshake(lib, akm, mfp)
    refused(lib)
    protected_frame_vectors(lib)


def pmf_on_hostap_aes(tmp):
    """Run test_pmf.rs (pmf.rs's vectors and Radio's driver against the register
    model) on the cipher the handset uses: hostap's AES and AES-CMAC through the
    same functions, instead of the test-only ones that `make test-unit` uses."""
    rustc = os.environ.get("HOST_RUSTC") or shutil.which("rustc")
    if not rustc:
        raise SystemExit("no rustc for the protected-frame tests on hostap's AES: set HOST_RUSTC")
    objects = []
    for name in ["crypto/aes-internal", "crypto/aes-internal-enc", "crypto/aes-omac1"]:
        objects.append(tmp / (Path(name).name + ".o"))
        subprocess.run([os.environ.get("CC", "cc"), "-O1", "-I", SRC, "-I", SRC / "utils",
                        "-DCONFIG_CRYPTO_INTERNAL", "-DCONFIG_INTERNAL_AES", "-DCONFIG_NO_STDOUT_DEBUG",
                        "-c", SRC / (name + ".c"), "-o", objects[-1]], check=True)
    subprocess.run(["ar", "rcs", tmp / "libhostap_aes.a", *objects], check=True)
    subprocess.run([rustc, "--edition=2021", "--test", "-O", "--cfg", "violet_hostap_aes",
                    "-L", tmp, "-l", "static=hostap_aes", HERE / "test_pmf.rs", "-o", tmp / "pmf"], check=True)
    subprocess.run([tmp / "pmf", "--quiet"], check=True)


if __name__ == "__main__":
    if not (SRC / "rsn_supp/wpa.c").is_file():
        raise SystemExit(f"hostap sources not found at {SRC}: run `make test-wpa` or set VIOLET_HOSTAP_SRC")
    with tempfile.TemporaryDirectory() as tmp:
        so = Path(tmp) / "station.so"
        # aes-ccm is only the reference for the protected-frame vectors; Radio does not link it.
        sources = [SRC / (name + ".c") for name in FILES + ["crypto/aes-ccm", "utils/os_unix", "utils/eloop", "utils/wpa_debug"]]
        subprocess.run([os.environ.get("CC", "cc"), "-shared", "-fPIC", "-O1", "-I", SRC,
                        "-I", SRC / "utils", "-DCONFIG_NO_STDOUT_DEBUG", "-DCONFIG_NO_RANDOM_POOL",
                        "-DCONFIG_CRYPTO_INTERNAL", "-DCONFIG_NO_RC4", "-DCONFIG_INTERNAL_AES", "-DCONFIG_INTERNAL_SHA1", "-DCONFIG_INTERNAL_SHA256",
                        "-DCONFIG_INTERNAL_MD5", "-DCONFIG_NO_WPA_MSG", "-DCONFIG_NO_WPA_TRACE",
                        *sources, HERE / "supplicant.c", HERE / "test_supplicant.c", "-o", so], check=True)
        test(c.CDLL(str(so)))
        print("Moto WPA2 four-way exchange (PSK and PSK-SHA256, with and without 802.11w), "
              "IGTK install and group rekey, invalid MIC, key contents, replay checks, "
              "and forged key frames on an established link passed")
        pmf_on_hostap_aes(Path(tmp))
