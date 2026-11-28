"""Collect real Sentinel traces for Violet Shield, scrubbed, on this computer.

docs/shield-data.md is the specification: what is kept, what is replaced, the
file layout and what the numbers can and cannot say. In short:

    python3 collect.py consent --corpus DIR --yes       opt in (handset traces need it)
    python3 collect.py ingest  --corpus DIR --source qemu|handset --label benign|attack|unlabelled
                               [--verdict exfil_or_c2|exploit_or_persistence] [--only APP,APP] RECORDS...
    python3 collect.py check   --corpus DIR             the corpus still follows the schema
    python3 collect.py report  --corpus DIR             false-positive figures with intervals
    python3 collect.py forget  --corpus DIR             delete everything collected

RECORDS are Sentinel record files: tab-separated lines (`t_ms pid app sha8 kind
flags idle_s detail target`, e.g. build/shield/qemu-traces/enforce-last.log) or
the JSON lines `companion_sim.py --record` writes.

Nothing here talks to a network. Traces from a handset are scrubbed before
they touch the disk: host names, paths, app names and hashes are replaced by
stand-ins of the same shape, made with a key kept OUTSIDE the corpus folder
(key_dir(): ~/.config/violet-shield/keys, or $VIOLET_SHIELD_KEYS), so a copy
of the folder does not carry what undoes the stand-ins. The stand-ins are
chosen so the on-device scorer computes exactly the same 24 features from a
scrubbed trace as from the original (`ingest` checks that for every record
and refuses to write otherwise). Emulator traces of the shipped apps hold
nothing personal and are kept as recorded; `--source qemu` is refused for a
capture holding any app, host or file name the shipped apps do not use.

The cross-app flag of every file record is set again from its path by
today's rule (`current_flags`, shield.rs `record`), so a capture made under
an older policy trains and reports as the phone behaves now.

Standard library only.
"""
from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import math
import os
import re
import shutil
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
import tiny  # noqa: E402

SCHEMA = "violet-shield-data-1"
SOURCES = ("qemu", "handset")
LABELS = ("benign", "attack", "unlabelled")
ATTACK_VERDICTS = ("exfil_or_c2", "exploit_or_persistence")
KINDS = ("EXEC", "EXIT", "OPEN_R", "OPEN_W", "SOCK_CONN", "SOCK_TX", "MEM_WX")
# Folders whose names are the system's, not the user's.
ROOTS = ("apps", "data", "tmp", "bin", "lib", "etc", "dev", "proc", "usr")
# The image's own lists in /apps (appstore::TITLES, manifest::MANIFESTS).
IMAGE_FILES = (".home", ".manifests")
# The rule the cross-app flag in a stored trace follows (shield.rs `record`):
# a corpus row made under another one is refused by `measure` and `real_rows`.
POLICY = "cross-app-2"
FLAG_CROSS = 2
# Owner names as the runtime makes them (appstore::valid_name, shield::owner_of).
OWNER = re.compile(r"[a-z0-9_-]{1,23}|~[0-9a-f]{8}|")
SHA8 = re.compile(r"[0-9a-f]{8}")
CONSENT_TEXT = (
    "I am adding Violet Shield traces from my own phone to a folder on this computer. They hold what my apps did "
    "(which kinds of files they opened, how much they sent, when), with names replaced by stand-ins. Lengths, "
    "counts, ports and timing are kept as they were. Nothing is sent anywhere. I can delete it all with "
    "`collect.py forget`."
)


def shipped_apps() -> set[str]:
    """Names of the apps in this repository: public, so kept as they are."""
    names = {"home", "settings", "weather"}
    for folder in ("apps/open", "apps/shield-demo", "targets/qemu/open-test-apps"):
        d = ROOT / folder
        if d.is_dir():
            names |= {p.name for p in d.iterdir() if p.is_dir()}
    return names


def app_folder(path: str):
    if not path.startswith("/apps/"):
        return None
    return path[6:].split("/")[0] or None


def cross_app(path: str, own: str) -> bool:
    """shield.rs `cross_app`: is `path` in another app's /data or /apps folder?"""
    if path.startswith("/data/"):
        rest = path[6:]
        first = rest.split("/")[0]
        if not first or first == own:
            return False
        flat = len(first) == len(rest)
        return not (flat and own and first.startswith(own) and first[len(own):].startswith("."))
    f = app_folder(path)
    return f is not None and f != own


def current_flags(r: tuple) -> tuple:
    """The record with its cross-app flag as today's policy sets it: a read of
    the image's lists (/apps/.home, /apps/.manifests) is nobody's data; any
    other read or write of another app's files is marked."""
    t, pid, app, sha8, kind, flags, idle, detail, target = r
    if kind not in ("OPEN_R", "OPEN_W"):
        return r
    write = kind == "OPEN_W"
    cross = cross_app(detail, app) and (write or detail not in ("/apps/" + f for f in IMAGE_FILES))
    return (t, pid, app, sha8, kind, flags & ~FLAG_CROSS | (FLAG_CROSS if cross else 0), idle, detail, target)


_TOKENS: set[str] | None = None


def shipped_tokens() -> set[str]:
    """Every name-like word in the shipped apps' and the runtime's sources: a
    name an emulator run of the image can produce is one of these."""
    global _TOKENS
    if _TOKENS is None:
        _TOKENS = set()
        for folder in ("apps/open", "apps/shield-demo", "targets/qemu/open-test-apps", "runtime/open"):
            for f in (ROOT / folder).rglob("*"):
                if f.is_file() and f.stat().st_size < 1 << 20:
                    _TOKENS |= set(re.findall(r"[A-Za-z0-9_.~-]+", f.read_text(errors="ignore")))
    return _TOKENS


def emulator_host(host: str) -> bool:
    host = host.lower()
    return host.endswith(".test") or host in ("localhost", "127.0.0.1", "10.0.2.2", "10.0.2.3") or host in shipped_tokens()


def emulator_problem(records) -> str | None:
    """Why `records` cannot be an emulator run of the shipped apps (None: they can)."""
    apps = shipped_apps()
    tokens = shipped_tokens()

    def path_ok(path: str) -> bool:
        parts = path.split("/")[1:]
        for depth, part in enumerate(parts):
            if depth == 0 and part not in ROOTS:
                return False
            if depth == 1 and parts[0] in ("apps", "data") and (part.partition(".")[0] in apps or part in IMAGE_FILES):
                continue
            if depth > 0 and part and part not in tokens:
                return False
        return True

    for r in records:
        app, sha8, kind, detail, target = r[2], r[3], r[4], r[7], r[8]
        if not (app in apps or re.fullmatch(r"~[0-9a-f]{8}", app)):
            return f"app {app!r}"
        for t in filter(None, (target, detail if kind == "SOCK_CONN" else "")):
            host = t.rpartition(":")[0] if ":" in t else t
            if not emulator_host(host):
                return f"host {host!r}"
        words = detail.split(" ") if kind == "EXEC" else [detail] if kind in ("OPEN_R", "OPEN_W") else []
        for w in filter(None, words):
            if not (path_ok(w) if w.startswith("/") else w in tokens):
                return f"name {w!r}"
    return None


class Scrubber:
    """Replace what could identify a person by stand-ins of the same shape.

    Letters become other letters, digits other digits; separators, lengths
    and the order of things stay. The same input gives the same stand-in
    inside one corpus (so "the same host twice" survives) and unrelated ones
    in another corpus.
    """

    def __init__(self, key: bytes, keep_names: set[str]):
        self.key = key
        self.keep = keep_names
        # (kind, stand-in) -> original, for app names and hosts: two that meet
        # in one stand-in anywhere in a capture are a `collision`.
        self.seen: dict[tuple, str] = {}
        self.collision = ""

    def _stream(self, kind: str, text: str):
        seed = hmac.new(self.key, f"{kind}\0{text}".encode(), hashlib.sha256).digest()
        n = 0
        while True:
            for b in hashlib.sha256(seed + n.to_bytes(4, "big")).digest():
                yield b
            n += 1

    def shape(self, kind: str, text: str) -> str:
        """`text` with every letter and digit replaced; the rest kept."""
        stream = self._stream(kind, text)
        out = []
        for ch in text:
            if ch.isascii() and ch.isdigit():
                out.append("0123456789"[next(stream) % 10])
            elif ch.isascii() and ch.isalpha():
                out.append("abcdefghijklmnopqrstuvwxyz"[next(stream) % 26])
            elif ch in ".-_/: ":
                out.append(ch)
            else:
                # Anything else (including non-ASCII, byte for byte) becomes letters.
                out.extend("abcdefghijklmnopqrstuvwxyz"[next(stream) % 26] for _ in ch.encode())
        out = "".join(out)
        if kind in ("app", "host") and self.seen.setdefault((kind, out), text) != text:
            self.collision = kind
        return out

    def name(self, app: str) -> str:
        if app.startswith("~"):
            # A program with no app folder: "~" and its hash.
            return "~" + self.shape("app", app[1:])
        return app if app in self.keep or not app else self.shape("app", app)

    def sha8(self, app: str, sha8: str) -> str:
        if app in self.keep:
            return sha8
        return hmac.new(self.key, b"sha\0" + sha8.encode(), hashlib.sha256).hexdigest()[:len(sha8)]

    def host(self, host: str) -> str:
        return self.shape("host", host.lower())

    def target(self, target: str) -> str:
        if not target:
            return target
        host, sep, port = target.rpartition(":")
        if not (sep and port.isascii() and port.isdigit() and len(port) <= 5 and int(port) <= 65535):
            return self.host(target)
        return self.host(host) + ":" + port

    def path(self, path: str) -> str:
        """An absolute path: system folders and shipped apps' names stay, the rest is stand-ins."""
        if not path.startswith("/"):
            return self.shape("word", path)
        parts = path.split("/")
        out = [""]
        for depth, part in enumerate(parts[1:]):
            if depth == 0:
                out.append(part if part in ROOTS else self.shape("dir", part))
            elif depth == 1 and parts[1] in ("apps", "data"):
                if part in IMAGE_FILES:
                    out.append(part)
                else:
                    # /data/NAME, /data/NAME.ext: the owner's name maps like the app column.
                    stem, dot, ext = part.partition(".")
                    out.append(self.name(stem) + dot + self.shape("file", ext) if stem else "." + self.shape("file", part[1:]))
            else:
                out.append(self.shape("file", part))
        return "/".join(out)

    def detail(self, kind: str, detail: str, target: str) -> str:
        if kind in ("SOCK_CONN",):
            return self.target(detail) if detail == target or ":" in detail else self.host(detail)
        if kind == "SOCK_TX":
            return detail  # "447B"
        if kind == "EXEC":
            return " ".join(self.path(word) if word.startswith("/") else self.shape("word", word) for word in detail.split(" "))
        if kind in ("OPEN_R", "OPEN_W"):
            return self.path(detail)
        if kind == "MEM_WX":
            return detail  # "0x0 len=65536"
        if kind == "EXIT":
            return detail  # "status 0"
        return self.shape("word", detail)

    def record(self, r: tuple) -> tuple:
        t, pid, app, sha8, kind, flags, idle, detail, target = r
        return (t, pid, self.name(app), self.sha8(app, sha8), kind, flags, idle, self.detail(kind, detail, target), self.target(target))


def parse_records(path: Path):
    """Records from a .log (tab separated) or .jsonl (companion_sim --record) file."""
    out = []
    for line in path.read_text(errors="replace").splitlines():
        if not line.strip():
            continue
        if line.lstrip().startswith("{"):
            try:
                d = json.loads(line)
                p = [d[k] for k in ("t_ms", "pid", "app", "sha8", "kind", "flags", "idle_s", "detail", "target")]
            except (ValueError, KeyError):
                continue
        else:
            p = line.split("\t")
            if len(p) != 9:
                continue
        try:
            out.append((int(p[0]), int(p[1]), str(p[2]), str(p[3]), str(p[4]), int(p[5]), int(p[6]), str(p[7]), str(p[8])))
        except ValueError:
            continue
    return out


def by_process(records):
    """{(pid, app, sha8): records}: one program run each, in order."""
    out: dict[tuple, list] = {}
    for r in records:
        out.setdefault((r[1], r[2], r[3]), []).append(r)
    return out


def feature_trail(records):
    """The scorer's features after each record that enters the window."""
    w = tiny.Window()
    trail = []
    for r in records:
        if w.add_record(r[4], r[5], r[6], r[7], r[8]):
            trail.append((tuple(w.features()), w.facts.crossed, w.facts.system_write, w.facts.tx_after))
    return trail


def line(r) -> str:
    return "\t".join(str(v) for v in r) + "\n"


def key_dir() -> Path:
    """Where scrubbing keys live: never inside a corpus, so a copy of a corpus
    folder is not also the means to undo its stand-ins."""
    if os.environ.get("VIOLET_SHIELD_KEYS"):
        return Path(os.environ["VIOLET_SHIELD_KEYS"])
    return Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "violet-shield" / "keys"


def corpus_id(corpus: Path, create: bool = True) -> str | None:
    """The corpus's random name for its key (says nothing about the key)."""
    f = corpus / "corpus-id"
    if not f.exists():
        if not create:
            return None
        f.write_text(os.urandom(16).hex() + "\n")
    cid = f.read_text().strip()
    if not re.fullmatch(r"[0-9a-f]{32}", cid):
        raise SystemExit(f"{f}: not a corpus id")
    return cid


def key_path(corpus: Path, create: bool = True) -> Path | None:
    cid = corpus_id(corpus, create)
    return key_dir() / f"{cid}.key" if cid else None


def _write_private(f: Path, data: bytes):
    fd = os.open(f, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as out:
        out.write(data)


def key_for(corpus: Path) -> bytes:
    """The corpus's scrubbing key, made on first use outside the corpus. A key
    an earlier version kept inside the corpus (.key) is moved out."""
    f = key_path(corpus)
    f.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    legacy = corpus / ".key"
    if legacy.exists():
        if f.exists():
            raise SystemExit(f"{legacy}: a second key inside the corpus; {f} is this corpus's key")
        _write_private(f, legacy.read_bytes())
        legacy.unlink()
    if not f.exists():
        _write_private(f, os.urandom(32))
    return f.read_bytes()


def consent(corpus: Path) -> bool:
    f = corpus / "consent.json"
    try:
        return json.loads(f.read_text()).get("text") == CONSENT_TEXT
    except (OSError, ValueError):
        return False


def manifest(corpus: Path):
    f = corpus / "manifest.jsonl"
    return [json.loads(l) for l in f.read_text().splitlines() if l.strip()] if f.exists() else []


def ingest(corpus: Path, source: str, label: str, verdict: str, files, now=None, only=None) -> list[dict]:
    """Add the processes in `files` (of the apps in `only`, if given) to the corpus;
    returns their manifest rows. Each capture is checked whole before any of
    it is written."""
    assert source in SOURCES and label in LABELS
    if label == "attack" and verdict not in ATTACK_VERDICTS:
        raise SystemExit("an attack trace needs --verdict " + " or ".join(ATTACK_VERDICTS))
    if source == "handset" and not consent(corpus):
        raise SystemExit("handset traces need consent first: collect.py consent --corpus DIR --yes")
    corpus.mkdir(parents=True, exist_ok=True)
    rows = manifest(corpus)
    added = []
    for path in files:
        records = [r for r in parse_records(Path(path)) if r[4] in KINDS]
        capture = hashlib.sha256(Path(path).read_bytes()).hexdigest()[:16]
        if not records:
            raise SystemExit(f"{path}: no Sentinel records")
        for r in records:
            if not OWNER.fullmatch(r[2]) or not SHA8.fullmatch(r[3]):
                raise SystemExit(f"{path}: pid {r[1]}: {r[2]!r}/{r[3]!r} is not an app name and hash the runtime makes")
        if source == "qemu":
            why = emulator_problem(records)
            if why:
                raise SystemExit(f"{path}: {why} is not one the shipped apps use, so this is not an emulator run of "
                                 "them; a phone's trace is --source handset (scrubbed, after consent)")
        scrub = Scrubber(key_for(corpus), shipped_apps()) if source == "handset" else None
        start = min(r[0] for r in records)
        pending = []
        for (pid, app, sha8), recs in by_process(records).items():
            if only and app not in only:
                continue
            recs = [current_flags((r[0] - start,) + r[1:]) for r in recs]
            kept = recs
            if scrub:
                kept = [scrub.record(r) for r in recs]
                if feature_trail(kept) != feature_trail(recs):
                    raise SystemExit(f"{path}: scrubbing pid {pid} would change what the scorer sees; nothing written")
            if any(m.get("capture") == capture and m.get("pid") == pid and m.get("sha8") == kept[0][3] for m in rows):
                continue
            pending.append((pid, app, kept))
        if scrub and scrub.collision:
            raise SystemExit(f"{path}: two different {scrub.collision} names share one stand-in; nothing written")
        if not pending:
            print(f"{path}: already in the corpus", file=sys.stderr)
            continue
        for pid, app, kept in pending:
            token = kept[0][2].lstrip("~") or "unnamed"
            n = 1 + sum(1 for m in rows if m["source"] == source and m["app"] == kept[0][2])
            rel = f"{label}/{source}-{token}-{n}.log"
            out = corpus / rel
            if not re.fullmatch(r"[a-z0-9_-]+", token) or out.resolve().parent != (corpus / label).resolve():
                raise SystemExit(f"{path}: {token!r} cannot name a file in the corpus")
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text("".join(line(r) for r in kept))
            row = {"schema": SCHEMA, "file": rel, "source": source, "label": label,
                   "verdict": verdict if label == "attack" else ("benign" if label == "benign" else ""),
                   "app": kept[0][2], "records": len(kept), "scrubbed": bool(scrub), "capture": capture,
                   # The process inside the capture (its hash as stored), so it is not added twice.
                   "pid": pid, "sha8": kept[0][3], "policy": POLICY,
                   # Only ever a name that was kept anyway (a shipped app's), else empty.
                   "original_app_kept": app if kept[0][2] == app else "",
                   "added": int(now if now is not None else time.time())}
            rows.append(row)
            added.append(row)
    (corpus / "manifest.jsonl").write_text("".join(json.dumps(r, sort_keys=True) + "\n" for r in rows))
    return added


def current_rows(corpus: Path):
    """The manifest, refusing rows whose flags follow an older policy."""
    rows = manifest(corpus)
    for m in rows:
        if m.get("policy") != POLICY:
            raise SystemExit(f"{m.get('file')}: recorded under policy {m.get('policy')!r}, not {POLICY}; "
                             "ingest its capture again into a new corpus")
    return rows


def check(corpus: Path) -> list[str]:
    """Problems with the corpus (an empty list: it follows the schema)."""
    problems = []
    listed = set()
    for m in manifest(corpus):
        listed.add(m.get("file"))
        if m.get("schema") != SCHEMA or m.get("source") not in SOURCES or m.get("label") not in LABELS:
            problems.append(f"{m.get('file')}: not a {SCHEMA} row")
            continue
        f = corpus / m["file"]
        if not f.is_file():
            problems.append(f"{m['file']}: missing")
            continue
        records = parse_records(f)
        if len(records) != m["records"] or len(records) != len([l for l in f.read_text().splitlines() if l]):
            problems.append(f"{m['file']}: {len(records)} records, the manifest says {m['records']}")
        if any(r[4] not in KINDS for r in records):
            problems.append(f"{m['file']}: a record kind outside the schema")
        if m["source"] == "handset" and not m.get("scrubbed"):
            problems.append(f"{m['file']}: a handset trace that was not scrubbed")
        if m.get("policy") != POLICY:
            problems.append(f"{m['file']}: flags follow policy {m.get('policy')!r}, not {POLICY}")
        if m["source"] == "qemu" and emulator_problem(records):
            problems.append(f"{m['file']}: an unscrubbed qemu trace with {emulator_problem(records)}")
    if (corpus / ".key").exists():
        problems.append(".key: the scrubbing key is inside the corpus (a corpus with its key is unscrubbed data)")
    for f in sorted(corpus.glob("*/*.log")):
        if str(f.relative_to(corpus)) not in listed:
            problems.append(f"{f.relative_to(corpus)}: not in the manifest")
    return problems


def wilson(k: int, n: int, z: float = 1.96):
    if n == 0:
        return 0.0, 1.0
    p = k / n
    d = 1 + z * z / n
    mid = (p + z * z / (2 * n)) / d
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return max(0.0, mid - half), min(1.0, mid + half)


def measure(corpus: Path, model=None):
    """What the on-device scorer says about every trace, per source and label.

    Counts processes, not windows: the windows of one run are far from
    independent, so a rate per window would look more certain than it is.
    A process counts as warned if any of its windows reaches the warning
    threshold, contained if one would be contained were the program one the
    phone may contain. This is the scorer alone (tiny.py, bit for bit the
    phone's); rate limits and built-in rules are the Rust replay's business
    (`make shield-fp-report`)."""
    model = model or tiny.Model.load()
    out: dict[tuple, dict] = {}
    for m in current_rows(corpus):
        cell = out.setdefault((m["source"], m["label"]), {"processes": 0, "windows": 0, "warned": 0, "contained": 0, "apps": {}})
        w = tiny.Window()
        warned = contained = False
        for r in parse_records(corpus / m["file"]):
            if not w.add_record(r[4], r[5], r[6], r[7], r[8]):
                continue
            f = w.features()
            action = tiny.decide(model, model.logits(f), f, True, w.facts)
            cell["windows"] += 1
            warned |= action >= tiny.WARN
            contained |= action >= tiny.QUARANTINE
        cell["processes"] += 1
        cell["warned"] += warned
        cell["contained"] += contained
        app = cell["apps"].setdefault(m["app"], [0, 0, 0])
        app[0] += 1
        app[1] += warned
        app[2] += contained
    return out


def rate(k: int, n: int) -> str:
    """k of n as a rate nobody can misread: never a bare "0%"."""
    low, high = wilson(k, n)
    if n == 0:
        return "no data"
    if k == 0:
        return f"0 of {n} (true rate could be up to {high * 100:.1f}%)"
    return f"{k} of {n} ({k / n * 100:.1f}%, 95% interval {low * 100:.1f}-{high * 100:.1f}%)"


def report(corpus: Path) -> str:
    cells = measure(corpus)
    lines = [f"corpus {corpus}: {sum(c['processes'] for c in cells.values())} processes; scorer {hashlib.sha256(tiny.MODEL_JSON.read_bytes()).hexdigest()[:8]}"]
    for (source, label), c in sorted(cells.items()):
        lines.append(f"{source} {label}: {c['processes']} processes, {c['windows']} windows scored")
        what = "false warnings" if label == "benign" else "warned"
        lines.append(f"  {what}: {rate(c['warned'], c['processes'])}")
        lines.append(f"  {'false containments' if label == 'benign' else 'would be contained'}: {rate(c['contained'], c['processes'])}")
        for app, (n, warned, contained) in sorted(c["apps"].items()):
            lines.append(f"    {app:<24} runs {n:3d}  warned {warned:3d}  contained {contained:3d}")
    benign = [c for (s, l), c in cells.items() if l == "benign"]
    n = sum(c["processes"] for c in benign)
    if n < 300:
        lines.append(f"note: {n} benign processes cannot show a false-warning rate under {wilson(0, n)[1] * 100:.0f}%; "
                     "about 3000 clean runs are needed to show under 0.13%.")
    return "\n".join(lines) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("command", choices=["consent", "ingest", "check", "report", "forget"])
    ap.add_argument("--corpus", default=str(ROOT / "build/shield/collected"))
    ap.add_argument("--source", choices=SOURCES)
    ap.add_argument("--label", choices=LABELS, default="unlabelled")
    ap.add_argument("--verdict", default="")
    ap.add_argument("--only", default="", help="ingest: only these apps' processes (comma separated)")
    ap.add_argument("--yes", action="store_true")
    ap.add_argument("files", nargs="*")
    args = ap.parse_intermixed_args(argv)
    corpus = Path(args.corpus)
    if args.command == "consent":
        print(CONSENT_TEXT)
        if not args.yes:
            print("\nRun again with --yes to agree.")
            return 1
        corpus.mkdir(parents=True, exist_ok=True)
        (corpus / "consent.json").write_text(json.dumps({"text": CONSENT_TEXT, "agreed": int(time.time())}, indent=1) + "\n")
        return 0
    if args.command == "ingest":
        if not args.source or not args.files:
            ap.error("ingest needs --source and at least one file")
        for row in ingest(corpus, args.source, args.label, args.verdict, args.files, only=set(filter(None, args.only.split(","))) or None):
            print(f"added {row['file']} ({row['records']} records{', scrubbed' if row['scrubbed'] else ''})")
        return 0
    if args.command == "check":
        problems = check(corpus)
        print("\n".join(problems) if problems else f"{len(manifest(corpus))} traces follow {SCHEMA}")
        return 1 if problems else 0
    if args.command == "report":
        sys.stdout.write(report(corpus))
        return 0
    if args.command == "forget":
        if not args.yes:
            print(f"This deletes {corpus} and everything in it. Run again with --yes.")
            return 1
        f = key_path(corpus, create=False) if corpus.is_dir() else None
        shutil.rmtree(corpus, ignore_errors=True)
        if f:
            f.unlink(missing_ok=True)
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main())
