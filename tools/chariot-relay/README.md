# The Chariot relay

[relay.py](relay.py) pairs a phone and a Mac in a room and forwards their messages ([protocol](../../docs/agent-protocol.md)). It never sees keys or plaintext. Standard library only.

```sh
python3 tools/chariot-relay/relay.py --port 8787          # on a local network
tools/chariot-relay/deploy.sh violet-chariot              # on a Chariot agent
```

The phone's default relay is the `violet-chariot` agent on chariots.sh (`RELAY` in [apps/open/agent/src/main.rs](../../apps/open/agent/src/main.rs)); a pairing carries its relay, so the Mac follows whatever the phone's code names.

What a Chariot does that matters here (learned November 2026):

- The app is a `chariot-agent-app/v1` (`.chariot-app/app.json` and `start` in the agent's workspace), served at `https://agent-<id without dashes>.apps.chariots.sh` behind Google's front end, HTTPS only.
- The app starts when the pod starts. After `deploy.sh`, restart it: `chariot hibernate violet-chariot`, then `chariot message violet-chariot ping` wakes it with the new code.
- The proxy answers WebSocket upgrades with 403 and ends a request after about 12 s, so clients poll (8 s holds) through it; `--poll-only` makes a local relay behave the same way for tests.
- Idle agents hibernate; `violet-chariot` is set to 90 days (`chariot hibernate-after set 90:00:00 --agent violet-chariot`). A hibernated agent answers 503 until a message wakes it.
- The zeroclaw image costs $0.20 a day.
