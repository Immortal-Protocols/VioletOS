# Violet Agent for macOS

The Mac half of the VioletOS voice agent. Open **Agent** on the phone, show its one-time code to this Mac's camera, and talk: the phone streams its microphone here, this app hears it with RoseOS's voice models, answers through Codex and speaks the reply on the phone in Rose's voice. Ask for an app ("Make me a simple scorekeeper") and Codex writes it, builds it in the app workspace and installs it on the phone, where it stays after a restart.

```sh
cd companion/mac
xcodegen generate
open VioletAgent.xcodeproj          # build and run in Xcode, or:
xcodebuild -scheme VioletAgent -configuration Release -derivedDataPath ../../build/mac-agent -allowProvisioningUpdates build
cp -R "../../build/mac-agent/Build/Products/Release/Violet Agent.app" ~/Applications/   # launch it from Finder
```

It needs Codex signed in with ChatGPT (`codex login`), the build tools of this checkout (`make moto` once, which installs the Rust target and the NDK it uses), and the RoseOS voice demo with its models (`bash scripts/setup.sh` in `RoseOS-1/prototypes/voice-phone-demo`; `ROSE_VOICE` points elsewhere). Without RoseOS it falls back to macOS speech recognition and synthesis (then macOS asks to allow **Speech Recognition**). macOS asks for the camera when you pair.

**Voice.** [voice/violet_voice.py](voice/violet_voice.py) runs in the RoseOS environment and uses its code unchanged: Rose's Listener (Silero VAD, smart-turn v3, Nemotron streaming ASR, Parakeet final transcripts, her endpointing rules) and Speaker (Phonon 31M, voice `zoey`), plus the voice half of her conductor (`is_backchannel`, `is_hold`, stop words, the duck → pause → stop timings and her echo check). The phone plays the speech like Rose's browser player and reports what it played, so an interruption knows what the user actually heard.

**Name.** The agent is called Violet until you rename it: **Rename…** in the window, `--name "My Dude"`, or just ask ("I'd like to call you My Dude"; Codex calls `rename_assistant`). The name is kept with the pairing keys and shown on the phone.

## How it fits together

| Piece | Where |
| --- | --- |
| Pairing, keys and the encrypted link | [Secure.swift](VioletAgent/Secure.swift), [PhoneLink.swift](VioletAgent/PhoneLink.swift), [docs/agent-protocol.md](../../docs/agent-protocol.md) |
| The relay connection (a Chariot, or a local relay) | [Relay.swift](VioletAgent/Relay.swift), [tools/chariot-relay](../../tools/chariot-relay) |
| Identity and paired phones, in the login Keychain | [Store.swift](VioletAgent/Store.swift) |
| RoseOS's voice (listener, Phonon speaker, barge-in) | [RoseVoice.swift](VioletAgent/RoseVoice.swift), [voice/violet_voice.py](voice/violet_voice.py) |
| macOS speech, when RoseOS is not installed | [Voice.swift](VioletAgent/Voice.swift) |
| Codex (`codex app-server`, sandboxed to the workspace) | [Codex.swift](VioletAgent/Codex.swift) |
| Turns, barge-in, installs and timings | [AgentModel.swift](VioletAgent/AgentModel.swift) |
| The camera scanner | [Scanner.swift](VioletAgent/Scanner.swift) |

Codex works in `build/agent-workspace` ([tools/agent_build.py](../../tools/agent_build.py)): the recipe in [AGENTS.md](../../tools/agent-workspace/AGENTS.md), a working native example and `./build NAME`, which builds offline from a warm cache. Codex runs with the `workspace-write` sandbox and no approvals, and installs only through the `install_app` tool this app provides.

**Interrupting.** Tap the circle on the phone to stop a reply. Talking over it works as in RoseOS: the voice ducks at once, pauses after 0.2 s of speech, and stops for real words or "wait"/"stop"; "mm-hmm", a cough or the agent's own voice coming back through the speakerphone let it carry on.

**Logs and timings** are in `~/Library/Application Support/Violet Agent`: `agent.log`, `status.json` and `timings.jsonl` (request, install and first open, in seconds).

## Testing without a person

| Flag | |
| --- | --- |
| `--pair CODE` | pair with the code a phone printed (`agent: pairing code …` in its log) instead of the camera |
| `--say TEXT` | send a typed request once the phone is online |
| `--install NAME=PATH` | install a built program once online |
| `--speak TEXT` | say a line on the phone |
| `--record-mic` | keep the phone's microphone stream as `mic.wav` |
| `--no-speech` | no voice at all |
| `--apple-speech` | macOS speech instead of RoseOS's |
| `--name NAME` | rename the agent |
| `--unpair`, `--unpair-remote` | forget the phone here, or tell the phone too |

`VIOLET_AGENT_PROFILE=test` keeps a separate identity; `make test-agent` uses it to pair with the phone in QEMU through a local relay, install a program, restart the phone, check the program is still there and runs, and unpair.
