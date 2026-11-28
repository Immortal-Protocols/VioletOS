"""Tests for the Shield data pipeline (collect.py, train_tiny.py --real).

    python3 tools/shield-trainer/test_collect.py

Standard library only, under a second, everything in a temporary folder.
Part of `make test-unit` (tools/shield-trainer/test_shield_data.sh).
"""
from __future__ import annotations

import json
import os
import re
import shutil
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
import collect  # noqa: E402
import tiny  # noqa: E402
import train_tiny  # noqa: E402

TRACES = ROOT / "targets/qemu/shield-traces"
# Scrubbing keys go to a folder of this run's own, never the user's.
KEYS = Path(tempfile.mkdtemp(prefix="shield-keys-"))
os.environ["VIOLET_SHIELD_KEYS"] = str(KEYS)

# What must never reach the disk from a handset: every name a person chose.
SECRETS = ["mybank", "taxes-2026", "passport", "alice", "clinic.example.org", "203.0.113.77", "vpn-7.corp.example.net",
           "Fotos", "résumé", "deadbeef", "journal", "secretapp"]

RAW = [
    # t_ms pid app sha8 kind flags idle_s detail target
    (5000, 210, "mybank", "deadbeef", "EXEC", 4, 0, "/apps/mybank", ""),
    (5100, 210, "mybank", "deadbeef", "OPEN_R", 4, 0, "/data/mybank/taxes-2026.pdf", ""),
    (5150, 210, "mybank", "deadbeef", "OPEN_W", 4, 0, "/data/mybank.cache", ""),
    (5200, 210, "mybank", "deadbeef", "OPEN_R", 6, 0, "/data/journal/alice/passport.jpg", ""),
    (5300, 210, "mybank", "deadbeef", "SOCK_CONN", 4, 1, "clinic.example.org:443", "clinic.example.org:443"),
    (5400, 210, "mybank", "deadbeef", "SOCK_TX", 5, 1, "70000B", "clinic.example.org:443"),
    (5500, 210, "mybank", "deadbeef", "SOCK_CONN", 5, 2, "203.0.113.77:8443", "203.0.113.77:8443"),
    (5600, 210, "mybank", "deadbeef", "SOCK_TX", 5, 2, "300B", "203.0.113.77:8443"),
    (5700, 210, "mybank", "deadbeef", "SOCK_CONN", 5, 2, "VPN-7.corp.example.net:443", "VPN-7.corp.example.net:443"),
    (5800, 210, "mybank", "deadbeef", "SOCK_CONN", 5, 3, "clinic.example.org:443", "clinic.example.org:443"),
    (5900, 210, "mybank", "deadbeef", "OPEN_W", 6, 3, "/apps/home/main", ""),
    (6000, 210, "mybank", "deadbeef", "OPEN_W", 4, 3, "/bin/python3", ""),
    (6100, 210, "mybank", "deadbeef", "MEM_WX", 4, 3, "0x0 len=65536", ""),
    (6200, 210, "mybank", "deadbeef", "EXEC", 4, 3, "/bin/python3 /data/mybank/Fotos/run.py --alice", ""),
    (6300, 210, "mybank", "deadbeef", "OPEN_R", 4, 4, "/data/mybank/résumé.txt", ""),
    (6400, 210, "mybank", "deadbeef", "EXIT", 4, 4, "status 0", ""),
    # Another program of the same capture: an unnamed one, and a shipped app.
    (6500, 211, "~0badcafe", "0badcafe", "EXEC", 4, 4, "/tmp/secretapp", ""),
    (6600, 211, "~0badcafe", "0badcafe", "SOCK_CONN", 4, 4, "clinic.example.org:443", "clinic.example.org:443"),
    (6700, 100, "home", "8ee0973b", "OPEN_R", 2, 5, "/apps/.home", ""),
    (6800, 100, "home", "8ee0973b", "SOCK_CONN", 0, 5, "wttr.in:443", "wttr.in:443"),
]


def write_raw(path: Path, records=RAW, as_json=False):
    keys = ("t_ms", "pid", "app", "sha8", "kind", "flags", "idle_s", "detail", "target")
    path.write_text("".join((json.dumps(dict(zip(keys, r))) + "\n") if as_json else collect.line(r) for r in records))


def test_scrubbing_keeps_the_scorers_view_and_no_names():
    scrub = collect.Scrubber(b"k" * 32, collect.shipped_apps())
    out = [scrub.record(r) for r in RAW]
    text = "".join(collect.line(r) for r in out)
    for secret in SECRETS:
        assert secret.lower() not in text.lower(), secret
    # The scorer computes the same features, record by record, for every process.
    for key, recs in collect.by_process(RAW).items():
        mapped = [scrub.record(r) for r in recs]
        assert collect.feature_trail(mapped) == collect.feature_trail(recs), key
        assert len(collect.feature_trail(recs)) >= 2
    mybank = [r for r in out if r[1] == 210]
    name = mybank[0][2]
    assert name != "mybank" and len(name) == 6 and name.isalpha()
    # Shape: lengths, separators and the folders that mean something stay.
    for raw, new in zip(RAW, out):
        assert (new[0], new[1], new[4], new[5], new[6]) == (raw[0], raw[1], raw[4], raw[5], raw[6])
        assert len(new[7].encode()) == len(raw[7].encode()) and len(new[8]) == len(raw[8])
    assert mybank[1][7].startswith(f"/data/{name}/") and mybank[2][7].startswith(f"/data/{name}."), "its own folder is still its own"
    assert mybank[3][7].startswith("/data/") and not mybank[3][7].startswith(f"/data/{name}"), "and another app's is still another's"
    assert mybank[10][7] == "/apps/home/" + mybank[10][7].rsplit("/", 1)[1] and mybank[11][7].startswith("/bin/")
    assert mybank[13][7].startswith("/bin/") and f" /data/{name}/" in mybank[13][7]
    assert mybank[4][8].endswith(":443") and mybank[6][8].endswith(":8443")
    assert mybank[4][8] == mybank[9][8] != mybank[8][8], "the same host is the same stand-in"
    assert all(c in "0123456789." for c in mybank[6][8].rsplit(":", 1)[0]), "an address stays an address"
    assert mybank[8][8] == mybank[8][8].lower()
    assert mybank[5][7] == "70000B" and mybank[12][7] == "0x0 len=65536" and mybank[15][7] == "status 0"
    # A shipped app keeps its name and hash; an unnamed program keeps its "~".
    home = [r for r in out if r[1] == 100]
    assert home[0][2:4] == ("home", "8ee0973b") and home[0][7] == "/apps/.home"
    unnamed = [r for r in out if r[1] == 211]
    assert unnamed[0][2].startswith("~") and unnamed[0][2] != "~0badcafe" and unnamed[0][3] != "0badcafe"
    assert unnamed[1][8] == mybank[4][8], "one corpus, one stand-in per host"
    # Same key, same stand-ins; another corpus, others.
    assert [scrub.record(r) for r in RAW] == out
    other = collect.Scrubber(b"j" * 32, collect.shipped_apps())
    assert other.record(RAW[4])[8] != out[4][8] and other.record(RAW[0])[2] != name


def test_ingest_needs_consent_and_writes_only_scrubbed_handset_data():
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        raw, corpus = tmp / "raw.log", tmp / "corpus"
        write_raw(raw)
        try:
            collect.ingest(corpus, "handset", "benign", "", [raw])
            raise AssertionError("ingested without consent")
        except SystemExit as e:
            assert "consent" in str(e)
        assert not corpus.exists() or not list(corpus.rglob("*.log"))
        assert collect.main(["consent", "--corpus", str(corpus)]) == 1 and not collect.consent(corpus)
        assert collect.main(["consent", "--corpus", str(corpus), "--yes"]) == 0 and collect.consent(corpus)
        # Consent to other words is not consent.
        (corpus / "consent.json").write_text(json.dumps({"text": "ok"}))
        assert not collect.consent(corpus)
        collect.main(["consent", "--corpus", str(corpus), "--yes"])
        rows = collect.ingest(corpus, "handset", "benign", "", [raw], now=1_800_000_000)
        assert len(rows) == 3 and all(r["scrubbed"] and r["schema"] == collect.SCHEMA and r["verdict"] == "benign" for r in rows)
        # Nothing a person chose is anywhere in the corpus folder.
        for f in corpus.rglob("*"):
            if f.is_file() and f.name != ".key":
                body = f.read_text().lower()
                for secret in SECRETS:
                    assert secret.lower() not in body and secret.lower() not in f.name.lower(), (f.name, secret)
        key = collect.key_path(corpus)
        assert key.stat().st_mode & 0o077 == 0, "the key is the owner's only"
        assert KEYS in key.parents and not (corpus / ".key").exists(), "the key is kept outside the corpus"
        assert collect.check(corpus) == []
        # Times start at the capture's first record; the same capture is not added twice.
        first = collect.parse_records(corpus / rows[0]["file"])[0]
        assert first[0] == 0
        assert collect.ingest(corpus, "handset", "benign", "", [raw]) == []
        assert collect.ingest(corpus, "handset", "benign", "", [raw], only={"mybank"}) == [], "nor one of its processes"
        # Home's read of the image's title list carries no cross-app mark now.
        home = [m for m in rows if m["app"] == "home"][0]
        assert collect.parse_records(corpus / home["file"])[0][5] == 0
        # The JSON-lines form companion_sim.py writes reads the same.
        as_json = tmp / "raw.jsonl"
        write_raw(as_json, RAW[:6], as_json=True)
        more = collect.ingest(corpus, "handset", "attack", "exfil_or_c2", [as_json])
        assert len(more) == 1 and more[0]["verdict"] == "exfil_or_c2" and more[0]["records"] == 6
        try:
            collect.ingest(corpus, "handset", "attack", "", [as_json])
            raise AssertionError("an attack without a verdict")
        except SystemExit:
            pass
        # check notices a trace that changed, one nobody listed, and an unscrubbed handset row.
        victim = corpus / rows[0]["file"]
        victim.write_text(victim.read_text().splitlines(keepends=True)[0])
        (corpus / "benign" / "stray.log").write_text("x\n")
        problems = collect.check(corpus)
        assert any("records" in p for p in problems) and any("stray.log" in p for p in problems), problems
        m = collect.manifest(corpus)
        m[1]["scrubbed"] = False
        (corpus / "manifest.jsonl").write_text("".join(json.dumps(r) + "\n" for r in m))
        assert any("not scrubbed" in p for p in collect.check(corpus))
        assert collect.main(["forget", "--corpus", str(corpus)]) == 1 and corpus.exists()
        assert collect.main(["forget", "--corpus", str(corpus), "--yes"]) == 0 and not corpus.exists()
        assert not key.exists(), "forget deletes the key too"


def test_scrubbing_that_would_change_the_features_is_refused():
    # Two one-letter hosts can meet in one stand-in; then "two destinations"
    # would read as one, and the ingest must stop.
    hosts = [(n, 300, "x", "aaaaaaaa", "SOCK_CONN", 4, 0, f"{h}:443", f"{h}:443") for n, h in enumerate("abcdefghijklmnopqrstuvwxyz")]
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        corpus = tmp / "c"
        collect.main(["consent", "--corpus", str(corpus), "--yes"])
        write_raw(tmp / "raw.log", hosts[:16])
        # A fixed key, so the test is the same every time.
        collect.key_path(corpus).parent.mkdir(parents=True, exist_ok=True)
        collect.key_path(corpus).write_bytes(b"k" * 32)
        scrub = collect.Scrubber(collect.key_for(corpus), set())
        assert len({scrub.target(r[8]) for r in hosts[:16]}) < 16, "with this key two of the hosts share a stand-in"
        try:
            collect.ingest(corpus, "handset", "benign", "", [tmp / "raw.log"])
            raise AssertionError("a trace whose features changed was written")
        except SystemExit as e:
            assert "would change" in str(e)
        assert not list(corpus.rglob("*.log")) and collect.manifest(corpus) == []


def real_corpus(tmp: Path) -> Path:
    """The checked-in emulator run as a corpus, the way `ingest` files it."""
    corpus = tmp / "real"
    capture = tmp / "enforce.log"
    capture.write_text("".join((TRACES / kind / f"qemu-enforce-{app}-1.log").read_text()
                               for kind, app in (("benign", "home"), ("benign", "notes"), ("attack", "photosync"), ("attack", "themes"))))
    collect.ingest(corpus, "qemu", "benign", "", [capture], only={"home", "notes"})
    collect.ingest(corpus, "qemu", "attack", "exfil_or_c2", [capture], only={"photosync"})
    collect.ingest(corpus, "qemu", "attack", "exploit_or_persistence", [capture], only={"themes"})
    return corpus


def test_emulator_traces_are_kept_as_recorded_and_reported_honestly():
    with tempfile.TemporaryDirectory() as tmp:
        corpus = real_corpus(Path(tmp))
        rows = collect.manifest(corpus)
        assert [(r["app"], r["label"], r["scrubbed"]) for r in rows] == [
            ("home", "benign", False), ("notes", "benign", False), ("photosync", "attack", False), ("themes", "attack", False)]
        for r in rows:
            kind = "benign" if r["label"] == "benign" else "attack"
            recorded = collect.parse_records(TRACES / kind / f"qemu-enforce-{r['app']}-1.log")
            stored = collect.parse_records(corpus / r["file"])
            # As recorded, except the cross-app flag, set again by today's rule:
            # Home's reads of /apps/.home were marked under the old one.
            assert stored == [collect.current_flags(x) for x in recorded]
            changed = [x for x, y in zip(recorded, stored) if x != y]
            assert all(x[2] == "home" and x[7] == "/apps/.home" and x[5] & 2 for x in changed)
            assert bool(changed) == (r["app"] == "home"), r["app"]
        assert collect.check(corpus) == []
        cells = collect.measure(corpus)
        benign, attack = cells[("qemu", "benign")], cells[("qemu", "attack")]
        # The scorer on the flags as today's phone sets them: no false warning.
        assert (benign["processes"], benign["warned"], benign["contained"]) == (2, 0, 0)
        assert benign["apps"]["home"] == [1, 0, 0] and benign["apps"]["notes"] == [1, 0, 0]
        assert (attack["processes"], attack["warned"], attack["contained"]) == (2, 2, 2)
        text = collect.report(corpus)
        assert "0 of 2 (true rate could be up to 65.8%)" in text, text
        assert not re.search(r"(?<![\d.])0(\.0+)?%", text), "a zero is never printed without its bound"
        assert "cannot show a false-warning rate under 66%" in text
    assert collect.rate(0, 0) == "no data"
    low, high = collect.wilson(0, 3000)
    assert low < 1e-9 and 0.0012 < high < 0.0013
    low, high = collect.wilson(5, 100)
    assert 0.021 < low < 0.022 and 0.111 < high < 0.113


def test_training_takes_real_runs_and_says_where_windows_came_from():
    with tempfile.TemporaryDirectory() as tmp:
        corpus = real_corpus(Path(tmp))
        real = train_tiny.real_rows(corpus)
        # Whole runs per split: of the two benign runs both train (the first two
        # of five); each attack run is one window, its last.
        families = sorted({f for rows in real.values() for f, _, _ in rows})
        assert families == ["real_qemu_attack", "real_qemu_benign"]
        assert len([1 for f, _, _ in real["train"] if f == "real_qemu_attack"]) == 2
        verdicts = {y for f, y, _ in real["train"] if f == "real_qemu_attack"}
        assert verdicts == {train_tiny.V["exfil_or_c2"], train_tiny.V["exploit_or_persistence"]}
        assert all(y == 0 for f, y, _ in real["train"] if f == "real_qemu_benign")
        assert real["calibration"] == [] and real["test"] == []
        synthetic, _ = train_tiny.dataset(3, 4)
        assert train_tiny.by_source(synthetic) == {"synthetic": len(synthetic)}
        mixed = train_tiny.by_source(synthetic + real["train"])
        assert mixed["real_qemu"] == len(real["train"]) and mixed["synthetic"] == len(synthetic)
        # A row is records the featurizer takes as they are.
        assert len(train_tiny.features(real["train"][0][2])) == tiny.F


def test_cross_app_flag_follows_todays_rule():
    r = lambda app, kind, path, flags=0: collect.current_flags((0, 1, app, "aaaaaaaa", kind, flags, 0, path, ""))[5]
    # Only the image's two lists are exempt, and only for reading.
    assert r("weather", "OPEN_R", "/apps/.home", 2 | 4) == 4 and r("weather", "OPEN_R", "/apps/.manifests") == 0
    assert r("weather", "OPEN_W", "/apps/.home") == 2
    for path in ("/apps/.agent-install", "/apps/.notes.part", "/apps/.home.part", "/apps/.homex", "/apps/home/main", "/data/notes/x"):
        assert r("weather", "OPEN_R", path) == 2, path
    for path in ("/apps/weather/main", "/data/weather.db", "/data/weather/x", "/tmp/x", "/lib/x"):
        assert r("weather", "OPEN_R", path, 2) == 0, path
    assert r("weather", "SOCK_TX", "/apps/.home", 2) == 2, "only file records are set again"
    # The same answers as shield.rs's own tests.
    assert collect.cross_app("/data/notes-pro.db", "notes") and not collect.cross_app("/data/notes.idx", "notes")
    assert collect.cross_app("/data/x", "") and not collect.cross_app("/data/", "notes") and not collect.cross_app("/apps/", "x")


def test_a_phone_capture_cannot_pass_as_an_emulator_one():
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        corpus = tmp / "c"
        for records, what in (
                (RAW, "app 'mybank'"),
                ([(0, 100, "home", "8ee0973b", "SOCK_CONN", 0, 0, "clinic.example.org:443", "clinic.example.org:443")], "host"),
                ([(0, 100, "notes", "8ee0973b", "OPEN_R", 0, 0, "/data/notes/taxes-2026.pdf", "")], "name"),
                ([(0, 100, "notes", "8ee0973b", "OPEN_R", 0, 0, "/home/alice/x", "")], "name"),
                ([(0, 100, "~0badcafe", "0badcafe", "EXEC", 0, 0, "/tmp/secretapp", "")], "name")):
            write_raw(tmp / "raw.log", records)
            try:
                collect.ingest(corpus, "qemu", "benign", "", [tmp / "raw.log"])
                raise AssertionError(f"{records[0]} accepted as an emulator run")
            except SystemExit as e:
                assert what in str(e) and "--source handset" in str(e), str(e)
            assert not list(corpus.rglob("*.log"))
        # check finds one written by hand.
        corpus.mkdir(exist_ok=True)
        (corpus / "benign").mkdir()
        (corpus / "benign" / "qemu-mybank-1.log").write_text(collect.line(RAW[0]))
        (corpus / "manifest.jsonl").write_text(json.dumps({"schema": collect.SCHEMA, "file": "benign/qemu-mybank-1.log", "source": "qemu",
                                                           "label": "benign", "records": 1, "policy": collect.POLICY}) + "\n")
        assert any("unscrubbed qemu" in p for p in collect.check(corpus))


def test_record_names_cannot_leave_the_corpus():
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        corpus = tmp / "deep" / "c"
        collect.main(["consent", "--corpus", str(corpus), "--yes"])
        for app, sha8 in (("../../../x", "aaaaaaaa"), ("a/b", "aaaaaaaa"), (".x", "aaaaaaaa"), ("~../x", "aaaaaaaa"), ("Bank", "aaaaaaaa"),
                          ("notes", "../../xx"), ("notes", "AAAAAAAA")):
            write_raw(tmp / "raw.jsonl", [(0, 5, app, sha8, "EXEC", 4, 0, "/apps/x", "")], as_json=True)
            for source in ("handset", "qemu"):
                try:
                    collect.ingest(corpus, source, "benign", "", [tmp / "raw.jsonl"])
                    raise AssertionError((app, sha8, source))
                except SystemExit as e:
                    assert "not an app name" in str(e), str(e)
        assert sorted(p.name for p in tmp.rglob("*") if p.is_file()) == ["consent.json", "raw.jsonl"]


def test_the_key_moves_out_and_collisions_across_processes_are_refused():
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        corpus = tmp / "c"
        collect.main(["consent", "--corpus", str(corpus), "--yes"])
        # A key an earlier version left inside the corpus is moved out, unchanged.
        (corpus / ".key").write_bytes(b"k" * 32)
        assert any(".key" in p for p in collect.check(corpus))
        assert collect.key_for(corpus) == b"k" * 32
        assert not (corpus / ".key").exists() and collect.key_path(corpus).read_bytes() == b"k" * 32
        # Two one-letter hosts that share a stand-in, each in its own process
        # (so neither process's features change): still refused.
        scrub = collect.Scrubber(b"k" * 32, set())
        by = {}
        for h in "abcdefghijklmnopqrstuvwxyz":
            by.setdefault(scrub.host(h), []).append(h)
        a, b = next(v for v in by.values() if len(v) > 1)[:2]
        recs = [(0, 300, "x", "aaaaaaaa", "SOCK_CONN", 4, 0, f"{a}:443", f"{a}:443"),
                (1, 301, "y", "bbbbbbbb", "SOCK_CONN", 4, 0, f"{b}:443", f"{b}:443")]
        write_raw(tmp / "raw.log", recs)
        try:
            collect.ingest(corpus, "handset", "benign", "", [tmp / "raw.log"])
            raise AssertionError("a shared stand-in was written")
        except SystemExit as e:
            assert "share one stand-in" in str(e), str(e)
        assert collect.manifest(corpus) == []


def test_only_and_old_policies():
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        corpus = tmp / "c"
        cap = tmp / "cap.log"
        cap.write_text("".join((TRACES / k / f"qemu-enforce-{a}-1.log").read_text() for k, a in (("benign", "home"), ("benign", "notes"))))
        assert [r["app"] for r in collect.ingest(corpus, "qemu", "benign", "", [cap], only={"home"})] == ["home"]
        assert collect.ingest(corpus, "qemu", "benign", "", [cap], only={"home"}) == []
        # A later ingest of the whole capture adds what the first left out, once.
        assert [r["app"] for r in collect.ingest(corpus, "qemu", "benign", "", [cap])] == ["notes"]
        assert collect.ingest(corpus, "qemu", "benign", "", [cap]) == []
        # A row made under an older cross-app rule is not trained on or reported.
        m = collect.manifest(corpus)
        del m[0]["policy"]
        (corpus / "manifest.jsonl").write_text("".join(json.dumps(r) + "\n" for r in m))
        assert any("policy" in p for p in collect.check(corpus))
        for f in (collect.measure, train_tiny.real_rows):
            try:
                f(corpus)
                raise AssertionError(f)
            except SystemExit as e:
                assert "policy" in str(e)


def test_runs_of_one_capture_share_a_split():
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        corpus = tmp / "c"
        # Five captures of two processes each: the third capture is the calibration one, the fifth the test one.
        for n in range(5):
            cap = tmp / f"cap{n}.log"
            cap.write_text(f"# {n}\n" + "".join((TRACES / "benign" / f"qemu-enforce-{a}-1.log").read_text() for a in ("home", "notes")))
            collect.ingest(corpus, "qemu", "benign", "", [cap])
        real = train_tiny.real_rows(corpus)
        runs = lambda rows: {len(recs) for _, _, recs in rows if len(recs) == 1}
        assert len(collect.manifest(corpus)) == 10
        assert runs(real["calibration"]) and runs(real["test"])
        by_split = {k: len(v) for k, v in real.items()}
        assert by_split["calibration"] == by_split["test"] and by_split["train"] == 3 * by_split["test"]


if __name__ == "__main__":
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    import contextlib
    import io
    for t in tests:
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            t()
    shutil.rmtree(KEYS, ignore_errors=True)
    print(f"test_collect.py: {len(tests)} tests passed")
