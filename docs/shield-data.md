# Violet Shield: real data

Violet Shield's on-device scorer was trained and measured on generated windows only. This page describes the pipeline that brings in real traces, how they are scrubbed, where they are kept, and what the resulting numbers can and cannot say. The scorer itself is described in [the proof of concept](violet-shield-poc.md).

## Where things stand

- **The shipped model is still trained on synthetic windows only.** The pipeline below exists and is tested; it has not yet been fed enough real data to retrain from.
- **Real traces in the repository: four processes from one emulator run** of the shipped image (Home, the notes demo, and the two attack demos), with complete records. They are in [targets/qemu/shield-traces](../targets/qemu/shield-traces) as `qemu-enforce-*.log` and `make test-unit` replays them through the real policy and scorer.
- **No handset traces from personal use exist.** Collecting them takes people using phones for weeks. The tools are ready for that; the data is not there.
- **The phone does not store traces.** The collection point is a computer the user owns: the Sentinel already sends its records to the paired companion, and `collect.py` scrubs them there before they are written. A switch on the phone that stops detail strings leaving it is not built.

Two real ordinary processes with no false positive leave the true rate anywhere up to 66% (95% confidence). Showing a false-warning rate under 0.13% takes about 3,000 clean, independent runs with none.

## What the first real trace found

On every boot the phone logged `shield 8: scorer says warn for pid 100`: a warning about Home. The complete trace showed why. Home reads `/apps/.home`, the image's list of app titles, every two seconds. The phone marked that as a read of another app's files, so Home's 447-byte weather request looked like "read someone's data, then sent it from the background".

The list is the image's and is nobody's private data. A read of exactly `/apps/.home` or `/apps/.manifests` (the image's two lists) is no longer marked as a cross-app read ([shield.rs](../runtime/open/shield.rs), `record`); a write to one still is, and is still refused. Every other `/apps/.NAME` stays marked: the Agent's staged program `/apps/.agent-install` and a developer push's `/apps/.NAME.part` are another app's program. The checked-in trace keeps the flags as they were recorded. [test_traces.rs](../tools/shield-trainer/test_traces.rs) replays it through today's policy and requires no warning in any mode; with the fix taken out, the same test reports one. `collect.py ingest` sets the cross-app flag of every file record again by today's rule (`current_flags`) and stamps the row with that rule's name (`policy`), so a capture made before the fix is not trained on or reported with the old mark; `report` and `train_tiny.py --real` refuse rows stamped with another rule.

The companion's large model also flagged Home once in an earlier run. It is shown the same cross-app mark, so the cause is probably the same; that has not been run again since the fix.

## The pipeline

Everything is in [tools/shield-trainer](../tools/shield-trainer), needs only Python's standard library, and never opens a network connection.

| Step | Command | What it does |
|---|---|---|
| Opt in | `collect.py consent --corpus DIR --yes` | Records agreement to the text below. Handset traces are refused without it. |
| Add traces | `collect.py ingest --corpus DIR --source qemu\|handset --label benign\|attack\|unlabelled [--verdict V] [--only APP,APP] FILE...` | Splits a capture into one file per process, scrubs handset data, writes the manifest. A process already in the corpus (same capture, pid and hash) is skipped, so `--only` runs can be completed later. |
| Verify | `collect.py check --corpus DIR` | The files match the manifest and the schema. |
| Measure | `collect.py report --corpus DIR` | False warnings and containments by the scorer, per source and label, with intervals. |
| Replay | `make shield-fp-report TRACES="DIR/benign/*.log"` | The same traces through the real policy, rate limits and scorer in Rust. |
| Train | `train_tiny.py --real DIR` | Adds labelled real runs to training and reports every split by source. |
| Delete | `collect.py forget --corpus DIR --yes` | Removes the corpus and its key (in the key folder). |

The default corpus is `build/shield/collected`, which Git ignores.

**Input.** A capture is a file of Sentinel records: tab-separated lines (`t_ms pid app sha8 kind flags idle_s detail target`), or the JSON lines `companion_sim.py --record` writes. `make test-shield-enforce` saves one for each run as `build/shield/qemu-traces/enforce-last.log`.

**The consent text.** "I am adding Violet Shield traces from my own phone to a folder on this computer. They hold what my apps did (which kinds of files they opened, how much they sent, when), with names replaced by stand-ins. Lengths, counts, ports and timing are kept as they were. Nothing is sent anywhere. I can delete it all with `collect.py forget`."

## Scrubbing

Handset traces are scrubbed before anything is written. Emulator traces of the shipped apps hold nothing personal (the hosts are `.test` stand-ins and wttr.in) and are kept as recorded, apart from the cross-app flag above. The record files carry no mark of where they were made, so `--source qemu` is checked: every app must be a shipped one (or an unnamed `~` program), every host a `.test` name, the emulator's own addresses or a host named in the shipped sources, and every file and command-line name one that appears in the shipped apps' or the runtime's sources. A capture that fails is refused and has to go in as `--source handset` (consent, scrubbing). This is a name check, not proof: a phone running only shipped apps on names those apps use passes, and then holds nothing beyond those names. `check` runs the same test on stored qemu rows.

| Field | Kept | Replaced |
|---|---|---|
| Time | Relative to the capture's first record | The absolute time |
| App name, hash | Names of apps in this repository | Every other name and hash, by a stand-in |
| Host | Length, where the dots and hyphens are, which characters are digits, the port | Every letter and digit |
| Path | `/apps`, `/data`, `/tmp`, `/bin`, `/lib`, `/etc`, `/dev`, `/proc`, `/usr`; shipped apps' folder names; `/apps/.home` and `/apps/.manifests`; lengths and separators | Every other folder and file name |
| Command line | The same as paths, word by word | Other words |
| Bytes sent, flags, idle time, process id | As recorded | |

A stand-in is made from the original with a random 32-byte key kept **outside** the corpus, in `~/.config/violet-shield/keys/ID.key` (`$XDG_CONFIG_HOME` or `$VIOLET_SHIELD_KEYS` move it; readable by its owner only), where ID is the random `corpus-id` in the corpus folder. The stand-ins are keyed, deterministic and keep lengths and separators, so anyone holding the key can confirm a guess at any scrubbed name: **a corpus together with its key is unscrubbed data.** Copying or sharing the corpus folder does not take the key along; a `.key` an earlier version left inside a corpus is moved out on the next ingest, and `check` reports one it finds. The same name gives the same stand-in inside one corpus, so "the same host twice" and "its own folder" survive; another corpus gives unrelated ones.

The stand-ins are chosen so the on-device scorer computes exactly the same 24 features from the scrubbed trace as from the original. `ingest` checks this for every record of every process and writes nothing if it does not hold. It also refuses a capture in which two different app names or two different hosts meet in one stand-in anywhere, in one process or across processes (that happens with very short names). Collisions with names from an earlier capture of the same corpus are not detected: the originals are not kept.

App names and hashes must look like what the runtime makes (`a-z 0-9 _ -`, at most 23 characters, or `~` and eight hex digits; an eight-hex-digit hash), and a file name built from one must land in the label's folder; anything else is refused before anything is written.

**What scrubbing does not hide.** Lengths of names, the number and timing of events, byte counts and ports all stay. Someone who knows which apps a person uses could match a trace to them. The corpus is for the computer it was collected on. Sharing one needs a separate review.

## Schema

`violet-shield-data-1`. A corpus is a folder:

```
DIR/consent.json            the agreed text and when
DIR/corpus-id               32 random hex digits: names the key, says nothing about it
DIR/manifest.jsonl          one row per trace
DIR/benign/SOURCE-APP-N.log
DIR/attack/SOURCE-APP-N.log
DIR/unlabelled/SOURCE-APP-N.log
```

A trace file is one process's records, oldest first, in the Sentinel's own line format, with these kinds: `EXEC`, `EXIT`, `OPEN_R`, `OPEN_W`, `SOCK_CONN`, `SOCK_TX`, `MEM_WX`. Records of what an earlier Shield did (`SCORE`, `SHIELD_HIT`, `QUARANTINED`) are left out: they are output.

A manifest row:

| Field | Meaning |
|---|---|
| `schema` | `violet-shield-data-1` |
| `file` | Path of the trace inside the corpus |
| `source` | `qemu` or `handset` |
| `label` | `benign`, `attack` or `unlabelled` |
| `verdict` | `benign`, `exfil_or_c2`, `exploit_or_persistence`, or empty when unlabelled |
| `app` | The app's name as stored (a stand-in for a scrubbed trace) |
| `records` | Number of records |
| `scrubbed` | Whether stand-ins were applied (always true for `handset`) |
| `capture` | First 16 hex digits of the capture file's SHA-256 |
| `pid`, `sha8` | The process inside the capture (its hash as stored): with `capture`, what is never added twice |
| `policy` | The cross-app rule the flags follow (`cross-app-2`: only reads of `/apps/.home` and `/apps/.manifests` are exempt) |
| `original_app_kept` | The app's name if it was kept, else empty |
| `added` | Unix time |

## Reading the numbers

- **Rates are per process, not per window.** The windows of one run overlap heavily. A rate per window would look far more certain than it is.
- **A zero is printed with its bound.** `0 of 2 (true rate could be up to 65.8%)`, never `0%`.
- **`collect.py report` is the scorer alone**, on the flags as stored, which `ingest` set by today's cross-app rule. `make shield-fp-report` and the tests replay through the whole policy, rate limits included. Both show no false warning for the checked-in Home trace.
- **Labels come from whoever ran `ingest`.** An app that provokes a "benign" label could poison training. Nothing here reviews labels.
- **Real runs are split by capture.** All processes of one capture (one boot) go to the same split: of every five captures, three train, one calibrates the thresholds and one is held out. The report's `windows_by_source` and `real_test` keep real figures apart from synthetic ones.

## What is missing

- A corpus worth training on: thousands of ordinary runs from real use, and real attack traces written for this system.
- Independent labels.
- Captures of the other shipped apps (Settings, Music, Ladybird, Agent, bitchat). They need an emulator run; the commands above take the result.
- Feedback from the Mac app. It writes its verdicts and the user's labels to files nothing reads yet.
