# Violet Agent for macOS

The Mac half of the VioletOS voice agent. **The agent is Rose**, RoseOS's voice agent, brought over whole: her listener and speaker, her Codex conversation with its barge-in, holds and heard-text record, her tasks and their announcements, her name and voices, her transcripts, and her life and journal. Choose **Pair a phone…**: either this Mac shows a one-time code and the phone reads it (**Scan the Mac's code** in Agent on the phone), or you show the phone's code to this Mac's camera. Then talk: the phone streams its microphone here, Rose hears it, answers and speaks on the phone. Ask for an app ("Make me a simple scorekeeper") and she starts a build task: Codex writes it, builds it in the app workspace and installs it on the phone, where it stays after a restart, and she tells you when it is ready.

```sh
cd companion/mac
xcodegen generate
open VioletAgent.xcodeproj          # build and run in Xcode, or:
xcodebuild -scheme VioletAgent -configuration Release -derivedDataPath ../../build/mac-agent -allowProvisioningUpdates build
cp -R "../../build/mac-agent/Build/Products/Release/Violet Agent.app" ~/Applications/   # launch it from Finder
```

Install Codex CLI 0.167.1 or later and Node (`brew install node`). In Violet Agent, click **Sign in with ChatGPT…** and finish `codex login` in the Terminal window that opens. Click **Restart sign-in…** to start over. Violet Agent starts Rose and the builder when sign-in completes. The companion keeps its Codex configuration and sign-in in `~/Library/Application Support/Violet Agent/Codex` and shares them across its phone profiles. Rose requires GPT 6.1 Terra Fast on the signed-in account.

Set up Rose's Python environment and voice models with `companion/mac/violet_rose/setup.sh`. It creates `build/rose-env`. Set `ROSE_ENV` to use a different environment. The app builder uses the build tools of this checkout, which `make moto` uses to build the handset image. macOS asks for camera access if you scan a pairing code with the Mac.

## Rose

[roseos](roseos) is Rose from RoseOS, unchanged, in RoseOS's own folder layout; [ROSE_VERSION](roseos/ROSE_VERSION) names the commit. `python3 tools/sync_rose.py` brings a newer Rose (`--rev`, `--rose` for another checkout). Violet's parts live beside her in [violet_rose](violet_rose) and never edit her files:

| | |
| --- | --- |
| [host.py](violet_rose/host.py) | runs Rose's own server (`rose.app.Server`: listener, speaker, the Codex conductor, tasks, identity, transcripts, life) for the phone, and serves her dashboard and HTTP API on 127.0.0.1 (the window links it) |
| [transport.py](violet_rose/transport.py) | Rose's audio transport with the phone at the far end: its microphone in, her speech to its player, the player's reports back |
| [conductor.py](violet_rose/conductor.py) | her Codex conductor plus VioletOS's app tools (`build_app` as one of her tasks, `open_app`, `list_apps`, `remove_app`); it withholds the tools whose far end is Rose Agent on Android, and keeps her to one app task at a time |
| [phone.py](violet_rose/phone.py) | the VioletOS phone as Rose's phone: her vision sessions (`phone_task`, her life's app work) see it by screenshots and work it by touch, and `look_at_screen` sees it; on the screen you see, since VioletOS has no hidden workspace |
| [RoseAgent.swift](VioletAgent/RoseAgent.swift) | starts the host and carries its frames: audio, her bus events, her calls and builds |

She keeps her state in the profile's `rose` folder: `identity.json` (her name and voice), `rosie-life/` (her life and journal), `transcripts/`. Her name starts as the one this app already used (Violet until you rename her: **Rename…** in the window, `--name`, or just ask). Interrupting works as in RoseOS: tap the circle on the phone, say "wait", or talk over her. `rose.log` in the support folder is her log.

## Connect the emulator on this Mac

Build the companion and run `make emulator` from the same checkout. In the emulator, open **Violet Agent** and click **Connect to this Mac**. The companion and emulator use the existing encrypted Agent pairing over a relay bound to Mac loopback. Each companion profile keeps one pairing. Use `VIOLET_AGENT_PROFILE=emulator` to keep your handset pairing in a separate profile.

The **Violet Shield** tab shows the Sentinel link and model verdicts. Signed warnings appear in the Shield badge on VioletOS Home. The Mac serves the emulator's Sentinel and TCP relay. Build with the Shield model assets in `build/shield` to enable AI scoring. Built-in checks can still warn when the model is unavailable. Use the same checkout and `VIOLET_SHIELD_KEY` (if set) for the VioletOS image and Mac companion so they share the pinned public key. A Moto paired with this Mac sends Shield telemetry over its encrypted Agent connection, including when Violet Relay on an iPhone provides Phone Internet.

For paired Agent telemetry, Sentinel polls the active TCP proxy over an authenticated SOCKS connection to `shield-flows.violet:7001`. The Mac and iPhone relays return the connecting peer’s targets, byte counts and flow ages. Sentinel forwards each snapshot with its boot-relative sampling time over the encrypted Agent session. The Mac matches it against kernel timestamps and writes the observer name to `shield-status.json` as `flow_observer`. Direct Wi-Fi, older relays and expired observations leave the cross-check unknown. A partial snapshot can corroborate a listed connection. Only a complete snapshot can establish a missing one.

## How it fits together

| Piece | Where |
| --- | --- |
| Pairing, keys and the encrypted link | [Secure.swift](VioletAgent/Secure.swift), [PhoneLink.swift](VioletAgent/PhoneLink.swift), [docs/agent-protocol.md](../../docs/agent-protocol.md) |
| The relay connection (a Chariot, or a local relay) | [Relay.swift](VioletAgent/Relay.swift), [tools/chariot-relay](../../tools/chariot-relay) |
| Identity and paired phones, in the login Keychain | [Store.swift](VioletAgent/Store.swift) |
| The agent: Rose | [RoseAgent.swift](VioletAgent/RoseAgent.swift), [violet_rose](violet_rose), [roseos](roseos) |
| The builder (`codex app-server`, sandboxed to the workspace) | [Codex.swift](VioletAgent/Codex.swift) |
| Rose's calls and builds, installs and timings | [AgentModel.swift](VioletAgent/AgentModel.swift) |
| The camera scanner, and the code this Mac shows | [Scanner.swift](VioletAgent/Scanner.swift), `PairSheet` in [VioletAgentApp.swift](VioletAgent/VioletAgentApp.swift) |
| The direct UDP path (STUN, hole punching, ordered delivery) | [Direct.swift](VioletAgent/Direct.swift) |

**Builds are Rose's tasks.** When you ask for an app she calls `build_app`, which starts one of her tasks and returns at once, so she keeps talking with you. This app's builder (a Codex thread in the workspace) carries it out and reports each step into the task; `list_tasks`, `update_task` and `cancel_task` work on it as on any of her tasks, and she announces the result when nobody is talking. `list_apps` and `remove_app` let you free the phone's app space by voice; she removes only apps you agreed to remove.

The builder works in `build/agent-workspace` ([tools/agent_build.py](../../tools/agent_build.py)): the recipe in [AGENTS.md](../../tools/agent-workspace/AGENTS.md), a working Rust example (`apps/example`), a working Python example (`python/tally`) and `./build NAME`. Rust builds run offline from a warm cache. Python apps use the [violet module](../../apps/lib/python/violet.py) (screen, touch, the Go fonts, sound, saved state); `./build` runs them once against a fake screen with taps across it, so errors show up before the install. The builder runs with the `workspace-write` sandbox and no approvals, and installs only through the `install_app` tool this app provides. Native apps build for AArch64, the phone's default; set `VIOLET_ARCH=aarch32` in the app's environment for the 32-bit system. The first Python app also installs Python on the phone (`build/python-package-aarch64.vgz`, or `build/python-package.vgz` on 32-bit, about 5 MB, made by [tools/build_python_package.py](../../tools/build_python_package.py) when missing).

**Direct path.** Once the phone is online the two look for a direct UDP path and use it when they find one; the window says *Direct path to the phone* or *Through the relay*. On a network that blocks UDP, or behind two strict NATs, everything keeps going through the relay. Details in [docs/agent-protocol.md](../../docs/agent-protocol.md#direct-path).

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
| `--say-after-install TEXT` | a typed turn once an app (not Python) is installed (tests) |
| `--install NAME=PATH` | install a built program once online (repeatable; a Python script installs Python first if the phone lacks it) |
| `--record-mic` | keep the phone's microphone stream as `mic.wav` |
| `--no-speech` | no agent at all (Rose is not started) |
| `--name NAME` | the agent's name, for a new Rose profile |
| `--unpair`, `--unpair-remote` | forget the phone here, or tell the phone too |

`VIOLET_AGENT_PROFILE=test` keeps a separate identity; `make test-agent` uses it to pair with the phone in QEMU through a local relay, install a program, restart the phone, check the program is still there and runs, and unpair. `make test-rose` runs Rose too: a typed request for a Python app becomes her build task, the app is installed on the phone in QEMU, she answers a question during the build and announces the result, each reply heard in full. `violet_rose/test_transport.py` checks the transport and `violet_rose/test_phone.py` Rose's vision session on a fake VioletOS phone, without Rose's server or a device (run them with `build/rose-env/bin/python`).

## Locomo Music

1. Build and open Violet Agent on an Apple-silicon Mac with macOS 15 or later. Click **+** next to the tabs, then **Set Up Locomo Music**. The companion downloads and verifies the [signed Locomo Music 0.1.14 release](https://github.com/wonderdawg/locomo-music-releases/releases/tag/v0.1.14), installs it in your Applications folder, and opens it to prepare its local models. Setup uses Homebrew to install ffmpeg if needed. The model download is about 10.9 GB and Locomo requires 16 GB free.
2. When setup finishes, the **Locomo Music** tab appears and stays enabled for that companion profile. Click **Enable One-Tap Generation…** and grant Violet Agent Accessibility access in System Settings if you want the companion to press Locomo’s controls automatically.
3. On a Moto running the updated VioletOS Agent and Music apps, connect the phone to Wi-Fi and pair **Violet Agent** with the Mac using the QR flow above. For QEMU, run `make emulator` from the same checkout, then open **Violet Agent → Connect to this Mac**. Launch the Mac companion with `VIOLET_AGENT_PROFILE=emulator` if you want to keep a separate handset pairing.
4. Return to Home and open **Locomo Music**. In **Generate**, choose a station and song length, then make one song or start an automatic run. **Overnight** arms Locomo's own batch when the Mac is on power and idle. The first finished track plays on arrival. The rest of an automatic run appears in the Mac library as Locomo finishes it.
5. In **Library**, search song titles or stations, browse **Favorites** or **Mac Library**, or choose **Cloud Songs** for the streamed Glass Orchard album. Tap a track to play it. **Refresh** fetches the latest page from the Mac.

Phone selections for song length and automatic song count update the corresponding Locomo preferences on the Mac. If the companion asks for a Mac-side click, apply those selections in Locomo Settings and start the chosen station. For a single song, click **Stop** once generation starts. The adapter reads the 0.1.14 library in read-only mode and sends audio over Agent's existing encrypted connection. **Remove Plugin** removes the tab and disables requests from the paired device. The Mac app, downloaded models, and songs stay on disk.

The smoke test uses a separate pairing and disposable QEMU disk. Build the Mac companion in Debug under `build/mac-agent` and run these commands from the repo root with Locomo ready.

```sh
make test-locomo LOCOMO_APP="/Applications/Locomo Music.app"
```
