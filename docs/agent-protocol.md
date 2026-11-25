# Violet Agent protocol

The phone's agent ([apps/open/agent](../apps/open/agent)) and the Mac companion ([companion/mac](../companion/mac)) talk through a relay, the **Chariot** ([tools/chariot-relay](../tools/chariot-relay)). Both make outbound connections; neither needs to reach the other's network. The relay pairs two connections and forwards their messages. It never sees keys or plaintext.

## Relay

`GET /v1/pipe/ROOM?role=phone|mac` with a WebSocket upgrade (RFC 6455), over HTTPS on a Chariot or plain HTTP on a local network. `ROOM` is 32 hex digits. A room holds one connection per role; a newer connection for a role replaces the older one.

- Binary messages are forwarded to the other role as they are, or dropped if it is not connected.
- The relay sends a text message `{"peer":"up"}` to both sides when the second one connects, and `{"peer":"down"}` to the one left when the other goes.
- Both sides ping every 20 s; the relay closes a connection silent for 60 s.
- `GET /health` answers `ok` and counts rooms, connections and forwarded messages.

**Plain HTTP.** A Chariot's proxy refuses WebSocket upgrades (403) and ends any request after about 12 s, so both ends fall back to polling when the upgrade is refused: `POST /v1/send/ROOM?role=R&id=ID` carries messages up and `GET /v1/recv/ROOM?role=R&id=ID` brings them down, held by the relay for up to 8 s. Each message in a body is a kind byte (1 text, 2 binary), a 4-byte big-endian length and the bytes. `ID` is random per connection: a new one is a new connection (and a `peer up`), and one that stops polling for 45 s is gone. The phone batches uploads every 200 ms; both ends simply ask again after a 5xx from the proxy.

## Identities and pairing

Each side has a long-term Ed25519 key. The phone keeps its seed and the paired Mac's public key in `/data/agent`; the Mac keeps its seed and the paired phones in the Keychain.

An unpaired phone shows a QR code holding

```
violet-agent:1?r=RELAY_URL&k=PHONE_KEY&s=SECRET&n=NAME
```

with base64url keys (no padding): `k` is the phone's Ed25519 public key and `s` a fresh 32-byte secret, used once. Both sides join the pairing room `hex(SHA-256("violet-agent pair" || s))[0..32]`. After a handshake in which the Mac proves it knows `s` and the phone's key matches `k`, the phone stores the Mac's key, forgets `s` and shows no code, and both move to the session room `hex(SHA-256("violet-agent room" || phone_key || mac_key))[0..32]`. From then on they meet there whenever both are online, from any network.

Unpairing on either side deletes the other's key and sends `{"t":"unpair"}` if connected. A Mac whose key the phone no longer holds cannot complete a handshake.

## Handshake

The phone leads. On `{"peer":"up"}`, and every 4 s while it has no session, it makes a new X25519 key and sends a HELLO. The Mac answers every phone HELLO with its own, naming the key it answers, and takes that session; the phone completes only with the answer to its latest offer, so crossed reconnects cannot leave the two with different keys. HELLO (binary):

| Bytes | |
| --- | --- |
| 1 | `0x01` |
| 1 | role: `p` (phone) or `m` (Mac) |
| 1 | mode: 0 session, 1 pairing |
| 32 | Ed25519 public key |
| 32 | X25519 ephemeral public key |
| 64 | Ed25519 signature of `"violet-agent hello v1" || role || mode || room (16 bytes) || ed || x || answer` |
| 32 | answer: the phone's X25519 key this Mac HELLO answers (zeros in the phone's) |
| 32 | pairing only, Mac only: HMAC-SHA256(s, `"violet-agent pair" || ed || x`) |

Keys: `HKDF-SHA256(salt = room, ikm = X25519(shared), info = "violet-agent v1" || phone_ed || mac_ed || phone_x || mac_x)`, 64 bytes: the phone-to-Mac key, then the Mac-to-phone key.

## Records

After the handshake every message is `0x02 || counter (8 bytes, big-endian) || ChaCha20-Poly1305(key, nonce = 0x00000000 || counter, plaintext)`. Counters start at 1 for each direction and key and must increase; anything else is dropped.

A plaintext is a kind byte and a body:

| Kind | Direction | Body |
| --- | --- | --- |
| `0x10` | both | a JSON object (below) |
| `0x20` | phone → Mac | microphone: 16 kHz s16le mono |
| `0x22` | phone → Mac | microphone: 16 kHz G.711 µ-law (what the phone sends) |
| `0x23` | phone → Mac | silence: a sample count (u32 LE) the phone did not send |
| `0x21` | Mac → phone | speech: segment id (u32 LE), then 24 kHz s16le mono (one piece) |
| `0x30` | Mac → phone | app install data: offset (u32 LE), then bytes |

**Microphone.** The phone's uplink through a relay cannot carry 32 KB/s, and silence needs none: a noise gate sends µ-law around speech (320 ms before it, 600 ms after) and a count of silent samples every 200 ms otherwise, so the Mac's endpointing, which counts samples, still sees time pass.

**Speech** is RoseOS's: the phone plays it like Rose's browser player. Mac → phone `{"t":"audio","ctl":{"type":"duck","gain":0.25}}` (and `unduck`, `pause`, `resume`, `stop`); phone → Mac `{"t":"audio_report","msg":{"type":"played","seg":N,"samples":S,"done":B}}` every 100 ms for the piece playing (samples within that piece, 24 kHz) and when it ends, and `{"type":"stopped","seg":N,"samples":S}` after a stop.

JSON messages carry `"t"`:

- phone → Mac: `hello` (name, version, `listening`), `stop` (the user tapped to interrupt), `audio_report`, `installed` (`name`, `ok`, `error`, `ms`), `opened` (`name`), `unpair`.
- Mac → phone: `hello` (`name`: the Mac's, `agent_name`: what the user calls the agent, "Violet" by default), `state` (`thinking`, `speaking`, `listening`, `building`), `transcript` (`who`: `user` or `agent`, `text`, `final`), `audio`, `say_stop`, `install` (`name`, `title`, `size` on the wire, `bytes` and `sha256` of the program, `encoding`: `deflate` or `none`), `install_end`, `open` (`name`), `unpair`.

An install arrives (raw DEFLATE when `encoding` says so), is inflated and checked against its length and SHA-256, written to `/apps/NAME`, and then the agent asks the Open Runtime (`install NAME TITLE` in `/dev/violet/sys`) to keep it on disk. Only an authenticated session can deliver one, so the phone accepts installs only from its paired Mac.
