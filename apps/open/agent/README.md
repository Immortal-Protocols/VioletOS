# Agent

The phone's half of the voice agent ([docs/agent-protocol.md](../../../docs/agent-protocol.md)); the Mac's is [Violet Agent](../../../companion/mac/README.md).

- **Unpaired**, it shows a one-time QR code (relay, its key, a secret) for the Mac's camera. Its identity and the paired Mac's key are kept in `/data/agent`, so restarts and network changes need no new scan. **Unpair** forgets the Mac.
- **Paired**, it streams the microphone (`/dev/violet/mic`, µ-law with a noise gate) to the Mac and plays the speech it gets back as RoseOS's player would. Tap the circle to interrupt, or to mute when nothing is playing. The header shows the agent's name.
- **Installs** come only through the authenticated session: the program is inflated, checked against its SHA-256, written to `/apps/NAME` and kept by the Open Runtime (`install NAME TITLE` in `/dev/violet/sys`, which only `/apps/agent` may use). It is listed in Programs and survives restarts and reflashes.

The relay is a Chariot (`RELAY` in [main.rs](src/main.rs), or `VIOLET_AGENT_RELAY` at build time); the shared setting `relay` overrides it, e.g. over USB: `build/moto-package "open-run=@sys set relay 192.168.1.20:8787"` for a relay on the local network (`tools/chariot-relay/relay.py`), and `set relay default` to go back. It talks WebSocket where it can and HTTP polling through proxies that refuse it.

`cargo test -p agent` (from `apps/`, for the Mac) checks the handshake, records, framing, µ-law and the QR drawing; `VIOLET_RELAY_TEST=https://… cargo test -p agent live -- --ignored` holds a polling link to a relay for two and a half minutes.
