# bitchat over Bluetooth LE: the parts VioletOS speaks

This summary is what [apps/open/bitchat](../apps/open/bitchat/README.md) implements. It was read from the apps' source on November 25, 2026:

- `permissionlesstech/bitchat`, commit 04995f6. The main files are `BinaryProtocol.swift`, `BitchatPacket.swift`, `MessagePadding.swift`, `Packets.swift`, `BLEService*.swift` and `BLEAnnounceHandler.swift`.
- `permissionlesstech/bitchat-android`, commit f6bfcea.

## Link

- **Service** `F47B5E2D-4A9E-4C5A-9B3F-8E1D2C3A4B5C`. Debug iOS builds use `…4B5A` and won't see release nodes.
- **One characteristic**, `A1B2C3D4-E5F6-4A5B-8C9D-0E1F2A3B4C5D`: notify, write, write without response, read.
- **Advertising** carries only the service UUID; there is no local name.
- **Roles.** Every node is both central and peripheral. There is no rule for who connects to whom, so two links between a pair are normal.
- **Data.** Centrals send with write without response; peripherals answer with notifications. Each write or notification carries exactly one bitchat frame.
- **MTU.** Centrals ask for a large ATT MTU (iOS 517). A frame that doesn't fit is split into fragments.

## Packet (version 1)

| Bytes | Field |
| --- | --- |
| 1 | version (1; 2 has a 4-byte length and source routes) |
| 1 | type |
| 1 | TTL (7 at the source) |
| 8 | timestamp, milliseconds since 1970 |
| 1 | flags: 0x01 recipient, 0x02 signature, 0x04 compressed, 0x08 route (v2), 0x10 sync response |
| 2 | payload length |
| 8 | sender ID: SHA-256 of the sender's X25519 Noise static key, first 8 bytes |
| 8 | recipient ID, if flagged (`FF×8` is broadcast) |
| n | payload. If compressed: a 2-byte original size, then raw DEFLATE |
| 64 | Ed25519 signature, if flagged |

- **Types used here:** 0x01 announce, 0x02 public message, 0x03 leave, 0x20 fragment. The app ignores 0x10/0x11 (Noise), 0x21 (sync) and the rest.
- **Signature.** Ed25519 over the packet encoded with TTL 0, no signature and the sync-response flag cleared, then padded.
- **Padding.** PKCS#7-style up to the smallest of 256, 512, 1024 or 2048 bytes that holds `length + 16`, but only when the padding fits in one byte. On the wire only Noise packets are padded.
- **Announce payload.** TLVs of type, one-byte length and value:
  - 0x01 nickname
  - 0x02 Noise key (32)
  - 0x03 Ed25519 key (32)
  - 0x04 neighbours
  - 0x05 capabilities: a little-endian bitfield, `05 01 00` for none
- **Receivers check:**
  - the sender ID matches the Noise key
  - the signature, against the key in the announce
  - the timestamp is within ±120 s of their clock
  - the keys stay the same for a known peer
- **An announce at TTL 7 means a direct neighbour.** Announce at least every 15–30 s.
- **Public message payload:** plain UTF-8, signed. Receivers need the sender's announce first.
- **Leave:** an empty payload, signed.
- **Fragment payload:** 8-byte ID, 2-byte index, 2-byte count, the original type, then a piece of the whole original frame. The fragment packets themselves aren't signed. The reassembled packet gets TTL 0.
- **Compression.** iOS compresses payloads of 100 bytes or more with under 90% distinct bytes, using Apple's raw DEFLATE. It verifies a signature by re-encoding the packet, so a signed payload that it would compress has to be compressed exactly as Apple's encoder does it. VioletOS sends messages under 100 bytes instead.
- **Relaying** (not done by VioletOS): TTL − 1, jittered, never for your own packets, sync requests or TTL ≤ 1.

## Private messages (not implemented)

- `Noise_XX_25519_ChaChaPoly_SHA256`, with an empty prologue and empty handshake payloads, in type 0x10 packets sent to the peer.
- Transport messages are type 0x11, built as:
  - a 4-byte big-endian nonce counter
  - then ChaCha20-Poly1305
  - the plaintext is a 1-byte payload type (0x01 private message, TLV of ID and text), then the data
