# Violet Agent protocol

The phone's agent ([apps/open/agent](../apps/open/agent)) and the Mac companion ([companion/mac](../companion/mac)) meet through a relay, the **Chariot** ([tools/chariot-relay](../tools/chariot-relay)). Both make outbound connections; neither needs to reach the other's network. The relay pairs two connections and forwards their messages. It never sees keys or plaintext. Once a session is up the two try to open a **direct path** over UDP (below) and move their records to it, keeping the relay connection as the fallback.

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

**The other way round.** The Mac can show the code instead (**Pair a phone…** › *Show this Mac's code*) and the phone read it with its rear camera (**Scan the Mac's code**: `/dev/violet/camera`, each frame in grey and decoded by `rqrr`). Same format, roles reversed, plus `o=m`:

```
violet-agent:1?r=RELAY_URL&k=MAC_KEY&s=SECRET&n=NAME&o=m
```

Values are percent-encoded. Both join the pairing room of `s` on the code's relay. The phone's HELLO carries the proof (the side that read the code proves it knows `s`); the Mac accepts any phone key with a valid proof, and the phone accepts only a Mac HELLO signed by `k`. The phone keeps the code's relay with the Mac's key and meets it there afterwards (the shared setting `relay` still overrides).

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
| 32 | pairing only, from the side that read the code (the Mac for the phone's code, the phone for the Mac's): HMAC-SHA256(s, `"violet-agent pair" || ed || x`) |

Keys: `HKDF-SHA256(salt = room, ikm = X25519(shared), info = "violet-agent v1" || phone_ed || mac_ed || phone_x || mac_x)`, 64 bytes: the phone-to-Mac key, then the Mac-to-phone key.

## Records

After the handshake every message is `0x02 || counter (8 bytes, big-endian) || ChaCha20-Poly1305(key, nonce = 0x00000000 || counter, plaintext)`. Counters start at 1 for each direction and key. Each counter is accepted once: one higher than any seen, or one of the 64 below the highest not seen yet (records cross when the path changes between the relay and the direct path); anything else is dropped.

A plaintext is a kind byte and a body:

| Kind | Direction | Body |
| --- | --- | --- |
| `0x10` | both | a JSON object (below) |
| `0x20` | phone → Mac | microphone: 16 kHz s16le mono |
| `0x22` | phone → Mac | microphone: 16 kHz G.711 µ-law (what the phone sends) |
| `0x23` | phone → Mac | silence: a sample count (u32 LE) the phone did not send |
| `0x21` | Mac → phone | speech: segment id (u32 LE), then 24 kHz s16le mono (one piece) |
| `0x30` | Mac → phone | app install data: offset (u32 LE), then bytes |
| `0x33` | Mac → QEMU | Locomo MP3 data: offset (u32 LE), then bytes |

**Microphone.** The phone's uplink through a relay cannot carry 32 KB/s, and silence needs none: a noise gate sends µ-law around speech (320 ms before it, 600 ms after) and a count of silent samples every 200 ms otherwise, so the Mac's endpointing, which counts samples, still sees time pass.

**Speech** is RoseOS's: the phone plays it like Rose's browser player. Mac → phone `{"t":"audio","ctl":{"type":"duck","gain":0.25}}` (and `unduck`, `pause`, `resume`, `stop`); phone → Mac `{"t":"audio_report","msg":{"type":"played","seg":N,"samples":S,"done":B}}` every 100 ms for the piece playing (samples within that piece, 24 kHz) and when it ends, and `{"type":"stopped","seg":N,"samples":S}` after a stop.

JSON messages carry `"t"`:

- phone → Mac: `hello` (name, version, `listening`, `python`: whether `/bin/python3` is there), `stop` (the user tapped to interrupt), `audio_report`, `installed` (`name`, `ok`, `error`, `ms`), `apps` (`apps`: `name`, `title`, `bytes` stored; `free` bytes; `python`), `uninstalled` (`name`, `ok`, `error`), `opened` (`name`), `unpair`, `direct` (below).
- Mac → phone: `direct` (below), `hello` (`name`: the Mac's, `agent_name`: what the user calls the agent, "Violet" by default), `state` (`thinking`, `speaking`, `listening`, `building`), `transcript` (`who`: `user` or `agent`, `text`, `final`), `audio`, `say_stop`, `install` (`name`, `title`, `size` on the wire, `bytes` and `sha256` of the program, `encoding`: `deflate` or `none`), `install_end`, `apps` (list them), `uninstall` (`name`), `open` (`name`), `unpair`.

An install arrives (raw DEFLATE when `encoding` says so), is inflated and checked against its length and SHA-256, written to `/apps/NAME` (a deflated program as `VZ1`, its length and the DEFLATE, which the Open Runtime keeps compressed and runs inflated), and then the agent asks the Open Runtime (`install NAME TITLE` in `/dev/violet/sys`) to keep it on disk. Only an authenticated session can deliver one, so the phone accepts installs only from its paired Mac.

A Python app is a `#!/bin/python3` script installed the same way. The handset image has no Python, so before the first one the Mac installs the package `python` ([tools/build_python_package.py](../tools/build_python_package.py)): data starting with `VGZ1` that the Open Runtime unpacks into `/bin` and `/lib` at every start rather than keeping as `/apps/python`. The phone's next `hello` says `python: true`.

**Locomo Music in QEMU.** The Mac advertises the enabled plugin as `locomo: true` in `hello` or sends `locomo_available` when it is enabled during the session. QEMU sends `locomo_make` with a request UUID in `id` and includes `resume: true` when reconnecting for that request. The Mac responds with `locomo_status` (`id`, `message`, `done`), or `locomo_track` (`id`, `title`, `durationMs`, `size`, `sha256`), MP3 records, then `locomo_end` with the request ID. QEMU publishes the file to Music after checking its SHA-256.

## Direct path

The relay is slow (the phone's uplink through a Chariot carries a few KB/s, each request goes through a proxy) and adds a round trip through the internet even when the phone and the Mac share a network. So after every handshake the two look for a direct UDP path ([direct.rs](../apps/open/agent/src/direct.rs), [Direct.swift](../companion/mac/VioletAgent/Direct.swift)); the Open Runtime gives apps UDP sockets for it ([runtime/open/sockets.rs](../runtime/open/sockets.rs), Radio's [sockets.c](../targets/moto-g-pure/ip/sockets.c)).

1. **Candidates.** Each side opens one UDP socket and asks STUN servers (RFC 5389 binding requests) for its public address and port: public ones by default (`stun.l.google.com:19302`, `stun.cloudflare.com:3478`), since a Chariot carries HTTPS only; the relay answers STUN itself when it runs with `--stun PORT` on a host that takes UDP. Candidates are those addresses plus each side's address on its own network.
2. **Exchange, inside the session.** The phone sends `{"t":"direct","key":HEX32,"cands":["ip:port",…]}` with a fresh random key; the Mac answers `{"t":"direct","cands":[…]}`. A failure on either side is `{"t":"direct","down":"why"}`.
3. **Punching.** Both send a PROBE to every candidate of the other's every 200 ms for 12 s. A PROBE is answered with a PROBE_ACK to wherever it came from; a side's path is open when an answer to one of its probes arrives (or data does), at the address it came from. Sending outward first opens each side's NAT to the other; the same network or one full-cone NAT is enough, two symmetric NATs are not.
4. **Records.** With the path open, all records go over it instead of the relay, cut into fragments of up to 1200 bytes, delivered in order: each datagram carries a fragment number and a cumulative acknowledgement, up to 12 fragments are in flight, and the oldest unacknowledged are sent again after three round trips (150 ms to 1 s). A side pings every 1.5 s when idle and gives the path up after 8 s without a datagram; records not yet acknowledged then go through the relay (the replay window drops any that did arrive), and the phone tries again after 30 s, doubling to 10 minutes. The relay connection stays up all along: the handshake, the exchange and the fallback use it.

Datagram, every one tagged with the session's direct key:

| Bytes | |
| --- | --- |
| 1 | `V` |
| 1 | kind: 1 PROBE, 2 PROBE_ACK, 3 DATA, 4 ACK, 5 PING |
| 4 | DATA: fragment number from 1; PROBE and PROBE_ACK: the probe's random number |
| 4 | the next fragment number expected from the other side |
| 1 | DATA: 1 on a record's last fragment |
| n | DATA: the fragment of a sealed record (`0x02 …` or a HELLO) |
| 16 | HMAC-SHA256(key, everything before), first 16 bytes |

Records stay sealed with the session's keys; the tag only keeps others from injecting, acknowledging or probing on the path. STUN replies arrive on the same socket and are told apart by their first byte (0 or 1).

**Turning it off.** `@sys set stun off` on the phone (or `@sys set stun HOST:PORT` for another STUN server), `--no-direct` or `VIOLET_AGENT_STUN=off` on the Mac. In QEMU `VIOLET_NET_UDP=0` makes the host relay refuse UDP, as a network that blocks it would; `make test-agent` checks both: an install over the direct path, then a boot on which only the relay works.
