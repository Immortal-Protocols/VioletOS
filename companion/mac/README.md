# Violet Agent for macOS

The Mac half of the VioletOS voice agent. Choose **Pair a phone…**: either this Mac shows a one-time code and the phone reads it (**Scan the Mac's code** in Agent on the phone), or you show the phone's code to this Mac's camera. Then talk: the phone streams its microphone here, this app hears it with RoseOS's voice models, answers through Codex and speaks the reply on the phone in Rose's voice. Ask for an app ("Make me a simple scorekeeper") and Codex writes it, builds it in the app workspace and installs it on the phone, where it stays after a restart.

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

## Connect the emulator on this Mac

Build the companion and run `make emulator` from the same checkout. In the emulator, open **Violet Agent** and click **Connect to this Mac**. The companion and emulator use the existing encrypted Agent pairing over a relay bound to Mac loopback. Each companion profile keeps one pairing. Use `VIOLET_AGENT_PROFILE=emulator` to keep your handset pairing in a separate profile.

The **Violet Shield** tab shows the Sentinel link and model verdicts. Signed warnings appear in the Shield badge on VioletOS Home. The Mac serves the emulator's Sentinel and TCP relay. Build with the Shield model assets in `build/shield` to enable AI scoring. Built-in checks can still warn when the model is unavailable. Use the same checkout and `VIOLET_SHIELD_KEY` (if set) for the VioletOS image and Mac companion so they share the pinned public key. A Moto paired with this Mac sends Shield telemetry over its encrypted Agent connection, including when Violet Relay on an iPhone provides Phone Internet.

## How it fits together

| Piece | Where |
| --- | --- |
| Pairing, keys and the encrypted link | [Secure.swift](VioletAgent/Secure.swift), [PhoneLink.swift](VioletAgent/PhoneLink.swift), [docs/agent-protocol.md](../../docs/agent-protocol.md) |
| The relay connection (a Chariot, or a local relay) | [Relay.swift](VioletAgent/Relay.swift), [tools/chariot-relay](../../tools/chariot-relay) |
| Identity and paired phones, in the login Keychain | [Store.swift](VioletAgent/Store.swift) |
| RoseOS's voice (listener, Phonon speaker, barge-in) | [RoseVoice.swift](VioletAgent/RoseVoice.swift), [voice/violet_voice.py](voice/violet_voice.py) |
| macOS speech, when RoseOS is not installed | [Voice.swift](VioletAgent/Voice.swift) |
| Codex (`codex app-server`: the talker, and the builder sandboxed to the workspace) | [Codex.swift](VioletAgent/Codex.swift) |
| Turns, barge-in, installs and timings | [AgentModel.swift](VioletAgent/AgentModel.swift) |
| The camera scanner, and the code this Mac shows | [Scanner.swift](VioletAgent/Scanner.swift), `PairSheet` in [VioletAgentApp.swift](VioletAgent/VioletAgentApp.swift) |
| The direct UDP path (STUN, hole punching, ordered delivery) | [Direct.swift](VioletAgent/Direct.swift) |

**Two Codex threads.** The *talker* holds the conversation: quick turns, a read-only sandbox, never writes code. When you ask for an app it calls `build_app` and goes on talking with you; the *builder* works in the background, and the talker learns how it is going (`build_status`) and hears when it is done (a `[builder]` message it passes on to you in a sentence). More requests during a build join it; `cancel_build` stops it. `list_apps` and `remove_app` let you free the phone's app space by voice; the talker removes only apps you agreed to remove.

The builder works in `build/agent-workspace` ([tools/agent_build.py](../../tools/agent_build.py)): the recipe in [AGENTS.md](../../tools/agent-workspace/AGENTS.md), a working Rust example (`apps/example`), a working Python example (`python/tally`) and `./build NAME`. Rust builds run offline from a warm cache. Python apps use the [violet module](../../apps/lib/python/violet.py) (screen, touch, the Go fonts, sound, saved state); `./build` runs them once against a fake screen with taps across it, so errors show up before the install. The builder runs with the `workspace-write` sandbox and no approvals, and installs only through the `install_app` tool this app provides. The first Python app also installs Python on the phone (`build/python-package.vgz`, about 5 MB, made by [tools/build_python_package.py](../../tools/build_python_package.py) when missing).

**Direct path.** Once the phone is online the two look for a direct UDP path and use it when they find one; the window says *Direct path to the phone* or *Through the relay*. On a network that blocks UDP, or behind two strict NATs, everything keeps going through the relay. Details in [docs/agent-protocol.md](../../docs/agent-protocol.md#direct-path).

**Interrupting.** Tap the circle on the phone to stop a reply. Talking over it works as in RoseOS: the voice ducks at once, pauses after 0.2 s of speech, and stops for real words or "wait"/"stop"; "mm-hmm", a cough or the agent's own voice coming back through the speakerphone let it carry on.

**Logs and timings** are in `~/Library/Application Support/Violet Agent`: `agent.log`, `status.json` and `timings.jsonl` (request, install and first open, in seconds).

**Shield training data.** The Mac companion appends each new verdict to `shield-verdicts.jsonl` in its Application Support profile directory. The file includes the canonical telemetry window and, for AI verdicts, the model input, side features, and class probabilities. Verdicts are predictions. Human labels remain in `shield-feedback.jsonl` and reference the verdict ID. Both files are local to the Mac.

## Testing without a person

| Flag | |
| --- | --- |
| `--pair CODE` | pair with the code a phone printed (`agent: pairing code …` in its log) instead of the camera |
| `--show-code` | show this Mac's code at once; it is also written to `pairing-code.txt` and `pairing-code.png` in the support folder until a phone pairs |
| `--relay URL` | the relay this Mac's own code names (else `VIOLET_AGENT_RELAY`, else the Chariot) |
| `--no-direct` | keep to the relay (`VIOLET_AGENT_STUN=off` does too; `VIOLET_AGENT_STUN=HOST:PORT` names another STUN server) |
| `--wait-direct` | with `--install`, install once the direct path is open (or after 20 s) |
| `--say TEXT` | send a typed request once the phone is online; repeat it for more, `--say-gap SECONDS` apart (10 by default), e.g. to ask questions during a build |
| `--install NAME=PATH` | install a built program once online (repeatable; a Python script installs Python first if the phone lacks it) |
| `--speak TEXT` | say a line on the phone |
| `--record-mic` | keep the phone's microphone stream as `mic.wav` |
| `--no-speech` | no voice at all |
| `--apple-speech` | macOS speech instead of RoseOS's |
| `--name NAME` | rename the agent |
| `--unpair`, `--unpair-remote` | forget the phone here, or tell the phone too |

`VIOLET_AGENT_PROFILE=test` keeps a separate identity; `make test-agent` uses it to pair with the phone in QEMU through a local relay, install a program, restart the phone, check the program is still there and runs, and unpair.

## Locomo Music in QEMU

1. On an Apple-silicon Mac with macOS 15 or later, install the [signed Locomo Music 0.1.14 release](https://github.com/wonderdawg/locomo-music-releases/releases/tag/v0.1.14) and run `brew install ffmpeg`. Open Locomo Music and finish its local model setup. The download is about 10.9 GB and setup requires 16 GB free.
2. In Locomo Music on the Mac, select **Cool Jazz** and turn off continuous generation so the station shows **Start**.
3. Build and open Violet Agent using the instructions above. In its **Locomo Music** tab, click **Choose Locomo Music…** and select the installed app. Choose it again after relaunching the companion.
4. From the same VioletOS checkout, run `make emulator`. In QEMU, open **Violet Agent → Connect to this Mac**. For a Mac that already has a handset paired, launch the companion from Terminal with `VIOLET_AGENT_PROFILE=emulator` to give QEMU its own pairing.
5. Return to QEMU Home, open **Locomo Music**, and tap **Make a Cool Jazz song**. The track plays on arrival. Tap **Latest** to replay it during the same QEMU boot.

For one-tap generation, enable **Violet Agent** under **System Settings → Privacy & Security → Accessibility** on the Mac. If the companion asks for a Mac-side click, click **Start** in Locomo's Cool Jazz station, then click **Stop** once generation has started to limit the run to one track. The adapter reads the 0.1.14 library in read-only mode and sends the finished audio over Agent's existing encrypted connection. The generation control is available in QEMU.

The smoke test uses a separate pairing and disposable QEMU disk. Build the Mac companion in Debug under `build/mac-agent` and run these commands from the repo root with Locomo ready.

```sh
make test-locomo LOCOMO_APP="/Applications/Locomo Music.app"
```
