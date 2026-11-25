# bitchat

Public chat with nearby phones running [bitchat](https://github.com/permissionlesstech/bitchat), over Bluetooth LE, with no internet in between. It talks to the iOS and Android apps in their public `#mesh` channel.

Open **bitchat** from Home. The status line shows:

- your nickname (`violet` and four hex digits, kept in `/data/bitchat` with your keys)
- how many people are in range

Tap the message box to type, then **Send**. Messages from others arrive with their nicknames. "joined" and "left" follow their announces.

## How it works

The app is a small Bluetooth LE host of its own, over `/dev/violet/hci` (one H4 packet per `read` or `write`; see [runtime/open/hci.rs](../../../runtime/open/hci.rs)).

**[ble.rs](src/ble.rs): the Bluetooth side.**
- Resets the controller, reads its buffer sizes and takes 16 random bytes (LE Rand). Those bytes, mixed with `/dev/urandom` and the time, seed a random static address and, on first start, the keys.
- Advertises the bitchat service, `F47B5E2D-4A9E-4C5A-9B3F-8E1D2C3A4B5C` (the apps' release UUID), and is named "VioletOS" in the scan response.
- Accepts connections.
- Serves a GATT database with the single bitchat characteristic `A1B2C3D4-E5F6-4A5B-8C9D-0E1F2A3B4C5D`: read, write, write without response, notify.
- Packets arrive as writes (long writes too) and leave as notifications. An L2CAP PDU larger than the controller's buffers is split, and sending follows the controller's buffer credits.
- Refuses pairing and L2CAP signalling requests.
- Also scans passively and logs bitchat devices it hears.

**[proto.rs](src/proto.rs): bitchat's packets** (see [docs/bitchat-protocol.md](../../../docs/bitchat-protocol.md)).
- Version 1 binary packets, big-endian, timestamps in milliseconds.
- Ed25519 signatures over the packet with TTL 0, padded to its block size.
- Announces carry the nickname, the X25519 "Noise" public key (the peer ID is the first 8 bytes of its SHA-256) and the Ed25519 signing key.
- Public messages are plain UTF-8.
- Fragments are reassembled, and deflated payloads inflated.

**[main.rs](src/main.rs): announcing, the screen and the clock.**
- Announces when a peer subscribes, when a new peer appears, and every 12 s.
- Checks every announce (its ID against its key, and its signature) and every message (against the sender's announced key). It drops its own and repeated packets.
- **The clock.** bitchat drops packets more than 2 minutes off its own clock. VioletOS has no real-time clock yet, so the app reads the time from an HTTPS server's `Date` header. Failing that, it takes the first peer's.

## Limits

- **Bluetooth runs only while Wi-Fi is joined**, or while Phone Internet is hosted. Radio turns the controller on with the connectivity chip ([bring-up notes](../../../targets/moto-g-pure/bluetooth-bringup.md)). Without it the app says so and waits.
- **Peripheral role only.** Peers connect to the phone; it doesn't connect to them, and it doesn't relay. It is a leaf of the mesh. Phones in range connect on their own, so chat works, but packets don't hop through VioletOS.
- **Public channel only.** There are no private messages (Noise XX), favourites, files or location channels.
- **Messages are at most 99 bytes.** iOS re-encodes a packet before checking its signature, and compresses payloads of 100 bytes or more. A longer message would have to match Apple's DEFLATE output byte for byte, so the app doesn't send one. Received messages of any length are fine.
- Only one app at a time can open `/dev/violet/hci`.

## Testing

- `make test-unit` runs the host tests for the packet format, the signatures, padding, fragments and the GATT server.
- Over the air, with the phone running bitchat:
  - `make bitchat-peer` builds a Mac peer (CoreBluetooth and CryptoKit, as the iOS app uses).
  - `build/BitchatPeer.app` connects to "VioletOS", checks its announce, and posts. With `--expect TEXT` it also waits for a message.
  - Its output goes to the file given to `open --stdout`. See [tools/bitchat-peer](../../../tools/bitchat-peer/main.swift).
  - macOS asks once for Bluetooth access. The build signs it with your Apple Development identity, so the grant survives rebuilds.
