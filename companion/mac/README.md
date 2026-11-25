# Violet Agent for macOS

The Mac half of the VioletOS voice agent. Open **Agent** on the phone, show its one-time code to this Mac's camera, and talk: the phone streams its microphone here, this app recognizes speech on the Mac, answers through Codex and speaks the reply on the phone. Ask for an app ("Make me a simple scorekeeper") and Codex writes it, builds it in the app workspace and installs it on the phone, where it stays after a restart.

```sh
cd companion/mac
xcodegen generate
open VioletAgent.xcodeproj          # build and run in Xcode, or:
xcodebuild -scheme VioletAgent -configuration Release -derivedDataPath ../../build/mac-agent -allowProvisioningUpdates build
cp -R "../../build/mac-agent/Build/Products/Release/Violet Agent.app" ~/Applications/   # launch it from Finder
```

It needs Codex signed in with ChatGPT (`codex login`) and the build tools of this checkout (`make moto` once, which installs the Rust target and the NDK it uses). On first launch macOS asks to allow **Speech Recognition**, and the camera when you pair.

## How it fits together

| Piece | Where |
| --- | --- |
| Pairing, keys and the encrypted link | [Secure.swift](VioletAgent/Secure.swift), [PhoneLink.swift](VioletAgent/PhoneLink.swift), [docs/agent-protocol.md](../../docs/agent-protocol.md) |
| The relay connection (a Chariot, or a local relay) | [Relay.swift](VioletAgent/Relay.swift), [tools/chariot-relay](../../tools/chariot-relay) |
| Identity and paired phones, in the login Keychain | [Store.swift](VioletAgent/Store.swift) |
| Speech recognition (on device) and synthesis (24 kHz to the phone) | [Voice.swift](VioletAgent/Voice.swift) |
| Codex (`codex app-server`, sandboxed to the workspace) | [Codex.swift](VioletAgent/Codex.swift) |
| Turns, barge-in, installs and timings | [AgentModel.swift](VioletAgent/AgentModel.swift) |
| The camera scanner | [Scanner.swift](VioletAgent/Scanner.swift) |

Codex works in `build/agent-workspace` ([tools/agent_build.py](../../tools/agent_build.py)): the recipe in [AGENTS.md](../../tools/agent-workspace/AGENTS.md), a working native example and `./build NAME`, which builds offline from a warm cache. Codex runs with the `workspace-write` sandbox and no approvals, and installs only through the `install_app` tool this app provides.

**Interrupting.** Tap the circle on the phone to stop a reply. While the phone is speaking, words the Mac hears that the agent is not saying itself (two or more) stop the reply too; the agent's own voice coming back through the phone's microphone is ignored.

**Logs and timings** are in `~/Library/Application Support/Violet Agent`: `agent.log`, `status.json` and `timings.jsonl` (request, install and first open, in seconds).

## Testing without a person

| Flag | |
| --- | --- |
| `--pair CODE` | pair with the code a phone printed (`agent: pairing code …` in its log) instead of the camera |
| `--say TEXT` | send a typed request once the phone is online |
| `--install NAME=PATH` | install a built program once online |
| `--speak TEXT` | say a line on the phone |
| `--record-mic` | keep the phone's microphone stream as `mic.wav` |
| `--no-speech` | skip speech recognition |
| `--unpair`, `--unpair-remote` | forget the phone here, or tell the phone too |

`VIOLET_AGENT_PROFILE=test` keeps a separate identity; `make test-agent` uses it to pair with the phone in QEMU through a local relay, install a program, restart the phone, check the program is still there and runs, and unpair.
