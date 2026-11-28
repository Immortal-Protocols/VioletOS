//! Host-only: replay recorded Sentinel traces through the real Shield code
//! and count what it would have done. Not part of the Open Runtime image.
//!
//! A trace is a file of record lines (`t_ms pid app sha8 kind flags idle_s
//! detail target`, as the Sentinel sends them; targets/qemu/shield-traces).
//! `replay` rebuilds each action, runs it through `ShieldPolicy::evaluate`
//! (baseline rules, quarantine, rate limits), feeds the events it records to
//! the scorer every 100 ms as the phone does, and reports per process what
//! was warned about, refused, quarantined or frozen.
//!
//! Two uses: the tests below (`make test-unit`) hold the checked-in traces to
//! a budget, and `make shield-fp-report` prints the false-positive rate, with
//! a confidence interval, for any traces you point it at.
use super::policy::{AccessMode, Action, AppId};
use super::scorer::{self, Ev, Scorer};
use super::shield::{self, EventKind, Mode, ShieldPolicy, ShieldVerdict, Subject, TelemetryEvent};
use std::string::String;
use std::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rec {
    pub t_ms: u64,
    pub pid: u32,
    pub name: String,
    pub sha8: String,
    pub kind: String,
    pub flags: u8,
    pub idle_s: u32,
    pub detail: String,
    pub target: String,
}

pub fn parse(line: &str) -> Option<Rec> {
    let p: Vec<&str> = line.trim_end_matches('\n').split('\t').collect();
    if p.len() != 9 {
        return None;
    }
    Some(Rec {
        t_ms: p[0].parse().ok()?,
        pid: p[1].parse().ok()?,
        name: p[2].into(),
        sha8: p[3].into(),
        kind: p[4].into(),
        flags: p[5].parse::<u32>().ok()? as u8,
        idle_s: p[6].parse().ok()?,
        detail: p[7].into(),
        target: p[8].into(),
    })
}

fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > 19 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// "host:port" as tiny.py splits it: a target without a valid port is all host.
pub fn split_target(target: &str) -> (&str, u16) {
    if let Some((host, port)) = target.rsplit_once(':') {
        if port.len() <= 5 {
            if let Some(p) = digits(port).filter(|p| *p <= 65535) {
                return (host, p as u16);
            }
        }
    }
    (target, 0)
}

fn sent_bytes(detail: &str) -> u32 {
    detail.strip_suffix('B').and_then(digits).map_or(0, |n| n.min(u64::from(u32::MAX)) as u32)
}

fn kind_code(kind: &str) -> Option<EventKind> {
    (1..=15).filter_map(shield::kind_from).find(|k| k.name() == kind)
}

/// The record as the phone's event (the inverse of `shield::record_line`).
pub fn to_event(r: &Rec) -> Option<TelemetryEvent> {
    let kind = kind_code(&r.kind)?;
    let now_ns = r.t_ms * 1_000_000;
    let app = AppId(r.pid);
    Some(match kind {
        EventKind::NetConnect | EventKind::NetTxSummary => {
            let (host, port) = split_target(&r.target);
            let mut ev = TelemetryEvent::new(now_ns, app, kind, r.flags, host.as_bytes());
            ev.port = port;
            ev.aux_u32 = sent_bytes(&r.detail);
            ev
        }
        _ => TelemetryEvent::new(now_ns, app, kind, r.flags, r.detail.as_bytes()),
    })
}

/// The record as a window event, straight from the text (as tiny.py does it).
pub fn ev_of(r: &Rec) -> Option<Ev> {
    let kind = kind_code(&r.kind)?;
    match kind {
        EventKind::NetConnect | EventKind::NetTxSummary => {
            let (host, port) = split_target(&r.target);
            Some(Ev::net(kind as u8, r.flags, host.as_bytes(), port, if kind == EventKind::NetTxSummary { sent_bytes(&r.detail) } else { 0 }))
        }
        EventKind::Exec | EventKind::OpenRead | EventKind::OpenWrite | EventKind::MemoryWx => Some(Ev::local(kind as u8, r.flags, &r.detail)),
        _ => None,
    }
}

/// How `replay` treats the programs in a trace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// As recorded: a program without the new-hash flag is part of the boot image.
    Recorded,
    /// As if the user had added every program (not Home, Settings or the Agent).
    Added,
}

/// Who a record's process is, through the same `Subject::derive` the phone
/// uses (guard.rs `subject`). A trace does not say where a program was
/// started from: it is taken to be the installed app /apps/NAME.
pub fn subject(r: &Rec, origin: Origin) -> Subject {
    let mut sha = [0u8; 4];
    for (k, b) in sha.iter_mut().enumerate() {
        *b = r.sha8.get(2 * k..2 * k + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).unwrap_or(0);
    }
    let path = std::format!("/apps/{}", r.name);
    let core = [shield::CORE_HOME, shield::CORE_SETTINGS, shield::CORE_INSTALLER].contains(&path.as_str());
    let image = r.flags & shield::FLAG_NEW_HASH == 0 && (origin == Origin::Recorded || core);
    let mut who = Subject::derive(r.pid, &path, &r.name, sha, image, r.flags & shield::FLAG_BACKGROUND == 0);
    who.flags |= r.flags & shield::FLAG_DEV_PUSH;
    who
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppReport {
    pub pid: u32,
    pub name: String,
    pub records: u32,
    /// Local warnings (the scorer said warn or more).
    pub warned: u32,
    /// "would-..." and "over-limit" reports: things only reported.
    pub shadow: u32,
    /// Calls refused with EACCES.
    pub denied: u32,
    pub quarantined: bool,
    pub frozen: bool,
    /// When it was first quarantined or frozen.
    pub contained_at_ms: Option<u64>,
}

impl AppReport {
    pub fn contained(&self) -> bool {
        self.quarantined || self.frozen
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub apps: Vec<AppReport>,
    /// Windows the scorer scored.
    pub scored: u32,
}

impl Report {
    pub fn contained(&self) -> usize {
        self.apps.iter().filter(|a| a.contained()).count()
    }
    pub fn warned(&self) -> usize {
        self.apps.iter().filter(|a| a.warned > 0).count()
    }
    pub fn denied(&self) -> u32 {
        self.apps.iter().map(|a| a.denied).sum()
    }
    pub fn merge(&mut self, other: Report) {
        self.apps.extend(other.apps);
        self.scored += other.scored;
    }
}

struct Run {
    policy: ShieldPolicy,
    scorer: Scorer,
    subjects: Vec<Subject>,
    report: Report,
    idle_s: u32,
}

impl Run {
    fn app(&mut self, pid: u32) -> &mut AppReport {
        let at = self.report.apps.iter().position(|a| a.pid == pid).expect("known pid");
        &mut self.report.apps[at]
    }

    /// The phone's 100 ms turn (guard.rs `shield_tick`).
    fn tick(&mut self, now_ns: u64) {
        self.policy.expire_quarantines(now_ns);
        let events = self.policy.ring.drain();
        for ev in &events {
            if ev.kind == EventKind::ShieldTriggered as u8 && (ev.detail_str().starts_with("would-") || ev.detail_str().starts_with("over-limit")) {
                if self.report.apps.iter().any(|a| a.pid == ev.app_id) {
                    self.app(ev.app_id).shadow += 1;
                }
            }
        }
        let subjects = self.subjects.clone();
        for local in self.scorer.step(&mut self.policy, &events, self.idle_s, now_ns, &subjects) {
            let a = self.app(local.pid);
            a.warned += 1;
            if local.enforced && local.action == scorer::FREEZE {
                a.frozen = true;
                a.contained_at_ms.get_or_insert(now_ns / 1_000_000);
            }
        }
        if let Some(n) = self.policy.notice.take() {
            let a = self.app(n.pid);
            a.quarantined = true;
            a.contained_at_ms.get_or_insert(now_ns / 1_000_000);
        }
    }
}

/// Replay one trace (the records of one boot, oldest first).
pub fn replay<'a>(lines: impl Iterator<Item = &'a str>, mode: Mode, origin: Origin) -> Report {
    let mut run = Run { policy: ShieldPolicy::with_mode(mode), scorer: Scorer::new(), subjects: Vec::new(), report: Report::default(), idle_s: 0 };
    let mut tick = None;
    let mut last_ms = 0;
    for r in lines.filter_map(parse) {
        let this = r.t_ms / 100;
        if let Some(t) = tick {
            // Empty turns in between do nothing but let quarantines end.
            if this > t {
                run.tick((t + 1) * 100_000_000);
            }
        }
        tick = Some(this.max(tick.unwrap_or(0)));
        last_ms = r.t_ms.max(last_ms);
        let now_ns = r.t_ms * 1_000_000;
        let who = subject(&r, origin);
        run.idle_s = r.idle_s;
        match run.subjects.iter_mut().find(|s| s.app == who.app) {
            Some(s) => *s = who,
            None => {
                run.subjects.push(who);
                run.report.apps.push(AppReport { pid: r.pid, name: r.name.clone(), ..AppReport::default() });
            }
        }
        run.app(r.pid).records += 1;
        let (host, port) = split_target(&r.target);
        let verdict = match r.kind.as_str() {
            "EXEC" => run.policy.evaluate(now_ns, &who, &Action::SpawnApp { path: &r.detail }),
            "OPEN_R" => run.policy.evaluate(now_ns, &who, &Action::OpenPath { path: &r.detail, mode: AccessMode::Read }),
            "OPEN_W" => run.policy.evaluate(now_ns, &who, &Action::OpenPath { path: &r.detail, mode: AccessMode::Write }),
            "SOCK_CONN" => {
                let mut udp = who;
                udp.flags |= r.flags & shield::FLAG_UDP;
                run.policy.evaluate(now_ns, &udp, &Action::Connect { host, port })
            }
            "MEM_WX" => {
                let len = r.detail.rsplit_once("len=").and_then(|(_, n)| n.parse().ok()).unwrap_or(0);
                run.policy.evaluate(now_ns, &who, &Action::MapWriteExec { addr: 0, len })
            }
            "SOCK_TX" => {
                // The runtime reports sends in summaries and counts them as they happen.
                if let Some(ev) = to_event(&r) {
                    run.policy.ring.emit(ev);
                }
                let mut left = sent_bytes(&r.detail);
                while left > 0 {
                    let chunk = left.min(64 * 1024);
                    run.policy.note_sent(now_ns, &who, chunk);
                    left -= chunk;
                }
                ShieldVerdict::Allow
            }
            "EXIT" => {
                run.policy.ring.emit(TelemetryEvent::new(now_ns, who.app, EventKind::Exit, 0, r.detail.as_bytes()));
                ShieldVerdict::Allow
            }
            // What an earlier Shield did (SHIELD_HIT, FROZEN, SCORE...) is output, not input.
            _ => ShieldVerdict::Allow,
        };
        if matches!(verdict, ShieldVerdict::Deny { .. }) {
            run.app(r.pid).denied += 1;
        }
        if let Some(n) = run.policy.notice.take() {
            let a = run.app(n.pid);
            a.quarantined = true;
            a.contained_at_ms.get_or_insert(r.t_ms);
        }
    }
    // Let the scorer get through every window that changed.
    for extra in 1..=(scorer::MAX_WINDOWS as u64) {
        run.tick((last_ms / 100 + extra) * 100_000_000);
    }
    run.report.scored = run.scorer.scored;
    run.report
}

/// 95% Wilson interval for `k` of `n`.
pub fn wilson(k: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let (k, n, z) = (k as f64, n as f64, 1.96f64);
    let p = k / n;
    let d = 1.0 + z * z / n;
    let mid = (p + z * z / (2.0 * n)) / d;
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / d;
    ((mid - half).max(0.0), (mid + half).min(1.0))
}

/// `shield-replay [--mode monitor|enforce|strict] [--added] FILE...`: every
/// file is one benign trace. Prints what each process got and the rates.
pub fn main() {
    let mut mode = Mode::Enforce;
    let mut origin = Origin::Recorded;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--mode" => mode = args.next().as_deref().and_then(Mode::parse).expect("--mode monitor|enforce|strict"),
            "--added" => origin = Origin::Added,
            _ => files.push(a),
        }
    }
    if files.is_empty() {
        std::eprintln!("usage: shield-replay [--mode monitor|enforce|strict] [--added] TRACE.log ...");
        std::process::exit(2);
    }
    let mut total = Report::default();
    std::println!("{:<34} {:>7} {:>6} {:>8} {:>6}  {}", "process", "records", "warned", "reported", "denied", "contained");
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {}", path, e));
        let report = replay(text.lines(), mode, origin);
        for a in &report.apps {
            let what = match (a.frozen, a.quarantined) {
                (true, _) => "FROZEN",
                (_, true) => "QUARANTINED",
                _ => "-",
            };
            let file = path.rsplit('/').next().unwrap_or(path);
            std::println!("{:<34} {:>7} {:>6} {:>8} {:>6}  {}", std::format!("{} ({})", a.name, file), a.records, a.warned, a.shadow, a.denied, what);
        }
        total.merge(report);
    }
    let n = total.apps.len();
    let line = |what: &str, k: usize| {
        let (low, high) = wilson(k, n);
        std::println!("{}: {} of {} processes; 95% interval {:.1}% to {:.1}%", what, k, n, low * 100.0, high * 100.0);
    };
    std::println!("mode {}, programs {}", mode.name(), if origin == Origin::Added { "treated as user-added" } else { "as recorded" });
    line("contained (quarantined or frozen)", total.contained());
    line("warned", total.warned());
    line("had a call refused", total.apps.iter().filter(|a| a.denied > 0).count());
    std::println!("windows scored: {}", total.scored);
    if n < 300 {
        std::println!("note: with {} processes the upper bound cannot go below {:.1}% even with no false positives.", n, wilson(0, n).1 * 100.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scorer::{decide, logits, Facts, Window, FREEZE, NONE, QUARANTINE, WARN};
    use crate::shield::AppInfo;
    use std::vec;

    const FIXTURES: &str = include_str!("../../tools/shield-trainer/fixtures/tiny_scorer.txt");

    fn rec(kind: &str, flags: u8, detail: &str, target: &str) -> Rec {
        Rec { t_ms: 0, pid: 100, name: "app".into(), sha8: "01020304".into(), kind: kind.into(), flags, idle_s: 0, detail: detail.into(), target: target.into() }
    }

    fn window(recs: &[Rec]) -> Window {
        let mut w = Window::new(100);
        for r in recs {
            if let Some(e) = ev_of(r) {
                w.add(e, r.idle_s);
            }
        }
        w
    }

    fn numbers<T: core::str::FromStr>(text: &str) -> Vec<T> {
        text.split(' ').map(|v| v.parse().ok().expect("number")).collect()
    }

    #[test]
    fn scorer_matches_python_reference() {
        let mut recs: Vec<Rec> = Vec::new();
        let mut features = [0u8; scorer::F];
        let mut facts = Facts::default();
        let (mut windows, mut actions) = (0, [0usize; 4]);
        for line in FIXTURES.lines() {
            let Some((tag, rest)) = line.split_once(' ') else { continue };
            match tag {
                "W" => recs.clear(),
                "R" => recs.push(parse(rest).expect("record")),
                "F" => {
                    let w = window(&recs);
                    features = w.features();
                    facts = w.facts;
                    assert_eq!(features.to_vec(), numbers::<u8>(rest), "features of window {}: {:?}", windows, recs);
                    // The phone builds the same window from its own events,
                    // which keep 40 bytes of a path, command line or host.
                    let mut device = Window::new(100);
                    for r in &recs {
                        let ev = to_event(r).expect("event");
                        assert_eq!(Ev::from_event(&ev), ev_of(r), "{:?}", r);
                        device.add(Ev::from_event(&ev).unwrap(), r.idle_s);
                    }
                    assert_eq!((device.features(), device.facts), (features, facts));
                }
                "L" => assert_eq!(logits(&features).to_vec(), numbers::<i32>(rest), "logits of window {}", windows),
                "A" => {
                    let action = decide(&logits(&features), &features, &facts, true);
                    assert_eq!(action, numbers::<u8>(rest)[0], "action of window {}", windows);
                    assert!(decide(&logits(&features), &features, &facts, false) <= WARN);
                    actions[action as usize] += 1;
                    windows += 1;
                }
                _ => {}
            }
        }
        assert!(windows >= 100, "{} fixture windows", windows);
        // The fixtures exercise every outcome, not just "nothing".
        assert!(actions.iter().all(|&n| n > 0), "{:?}", actions);
    }

    #[test]
    fn record_lines_round_trip() {
        let info = AppInfo { name: "photosync".into(), sha8: "97f769a4".into() };
        for line in [
            "7696\t107\tphotosync\t97f769a4\tSOCK_CONN\t1\t0\tdrop.c2-exfil.test:4444\tdrop.c2-exfil.test:4444",
            "7721\t107\tphotosync\t97f769a4\tSOCK_TX\t1\t3\t458816B\tdrop.c2-exfil.test:4444",
            "1658\t107\tphotosync\t97f769a4\tOPEN_R\t2\t0\t/data/notes/keys.txt\t",
            "1599\t107\tphotosync\t97f769a4\tMEM_WX\t0\t0\t0x0 len=65536\t",
            "1000\t107\tphotosync\t97f769a4\tEXEC\t5\t9\t/bin/python3 /apps/photosync/main.py\t",
        ] {
            let r = parse(line).unwrap();
            assert_eq!(shield::record_line(&to_event(&r).unwrap(), Some(&info), r.idle_s), line);
        }
        assert!(parse("not a record").is_none());
        assert_eq!(split_target("[::1]:80"), ("[::1]", 80));
        assert_eq!(split_target("host:+80"), ("host:+80", 0));
        assert_eq!(split_target("host:99999"), ("host:99999", 0));
        assert_eq!(split_target(""), ("", 0));
    }

    #[test]
    fn window_is_bounded_and_merges_sends() {
        let mut w = Window::new(1);
        for n in 0..40 {
            w.add(ev_of(&rec("OPEN_R", 2, &std::format!("/data/notes/{}", n), "")).unwrap(), 0);
        }
        assert_eq!(w.features()[scorer::F_EVENTS], 16);
        assert_eq!(w.features()[scorer::F_XREAD], 15, "counts stop at 15");
        // A flood of sends to one place is one event and cannot push the reads out.
        w.add(ev_of(&rec("SOCK_CONN", 1, "a.test:443", "a.test:443")).unwrap(), 0);
        for _ in 0..1000 {
            w.add(ev_of(&rec("SOCK_TX", 1, "65536B", "a.test:443")).unwrap(), 0);
        }
        let f = w.features();
        assert_eq!((f[scorer::F_EVENTS], f[scorer::F_XREAD], f[scorer::F_DEST]), (16, 14, 1));
        assert_eq!(f[scorer::F_TX], 26, "65,536,000 bytes");
        for _ in 0..100_000 {
            w.add(ev_of(&rec("SOCK_TX", 1, "4294967295B", "a.test:443")).unwrap(), 0);
        }
        assert_eq!(w.features()[scorer::F_TX], 32, "saturates, never wraps");
        // A send somewhere else is a new event.
        w.add(ev_of(&rec("SOCK_TX", 1, "10B", "b.test:443")).unwrap(), 0);
        assert_eq!(w.features()[scorer::F_DEST], 2);
    }

    fn exfil(flags: u8) -> Vec<Rec> {
        vec![
            rec("EXEC", flags, "/apps/photosync", ""),
            rec("OPEN_R", flags | 2, "/data/notes/keys.txt", ""),
            rec("SOCK_CONN", flags | 1, "185.220.101.4:4444", "185.220.101.4:4444"),
            rec("SOCK_TX", flags | 1, "1048576B", "185.220.101.4:4444"),
        ]
    }

    #[test]
    fn the_model_alone_never_contains() {
        // Exfiltration by a program the user added: two signs, quarantine.
        let w = window(&exfil(4));
        let (f, l) = (w.features(), logits(&w.features()));
        assert_eq!(decide(&l, &f, &w.facts, true), QUARANTINE);
        // The same score for a program the phone may not contain: a warning.
        assert_eq!(decide(&l, &f, &w.facts, false), WARN);
        // A W+X map and a connection to a bare address look bad to the model,
        // but there is no second sign: a warning, whatever the margin.
        let w = window(&[rec("EXEC", 5, "/apps/x", ""), rec("MEM_WX", 5, "0x0 len=65536", ""), rec("SOCK_CONN", 5, "45.137.21.9:1337", "45.137.21.9:1337")]);
        let (f, l) = (w.features(), logits(&w.features()));
        assert!(scorer::margin(&l) >= crate::scorer_model::T_CONTAIN, "the model is sure: {:?}", l);
        assert_eq!(decide(&l, &f, &w.facts, true), WARN);
        // Overwriting another app's program: frozen.
        let w = window(&[rec("EXEC", 5, "/apps/themes", ""), rec("MEM_WX", 5, "0x0 len=65536", ""), rec("OPEN_W", 7, "/apps/home/main", "")]);
        assert_eq!(decide(&logits(&w.features()), &w.features(), &w.facts, true), FREEZE);
        // An ordinary app.
        let w = window(&[rec("EXEC", 0, "/apps/notes", ""), rec("OPEN_W", 0, "/data/notes/todo.txt", ""), rec("SOCK_CONN", 0, "sync.notes.test:443", "sync.notes.test:443"), rec("SOCK_TX", 0, "85B", "sync.notes.test:443")]);
        assert_eq!(decide(&logits(&w.features()), &w.features(), &w.facts, true), NONE);
    }

    #[test]
    fn filler_events_cannot_push_the_evidence_out() {
        use scorer::{corroborated, F_EVENTS, F_SYSW, F_TX_AFTER, F_XREAD};
        let upload = [rec("SOCK_CONN", 5, "185.220.101.4:4444", "185.220.101.4:4444"), rec("SOCK_TX", 5, "1048576B", "185.220.101.4:4444")];
        // Read another app's data, open 40 of its own files, then upload.
        let mut recs = vec![rec("EXEC", 4, "/apps/photosync", ""), rec("OPEN_R", 6, "/data/notes/keys.txt", "")];
        for n in 0..40 {
            recs.push(rec(if n % 2 == 0 { "OPEN_R" } else { "OPEN_W" }, 4, &std::format!("/data/photosync/{}", n), ""));
        }
        recs.extend(upload.iter().cloned());
        let w = window(&recs);
        let f = w.features();
        assert_eq!((f[F_EVENTS], f[F_XREAD]), (16, 1), "the read is still in the window");
        assert!(f[F_TX_AFTER] >= 17 && corroborated(&f, &w.facts));
        assert_eq!(decide(&logits(&f), &f, &w.facts, true), QUARANTINE);
        // Sixteen events that do count (connections) push the read out; that it
        // happened is kept, and the send still counts as coming after it.
        let mut recs = vec![rec("OPEN_R", 6, "/data/notes/keys.txt", "")];
        for n in 0..16 {
            let host = std::format!("h{}.test:443", n);
            recs.push(rec("SOCK_CONN", 5, &host, &host));
        }
        recs.push(upload[1].clone());
        let w = window(&recs);
        let f = w.features();
        assert_eq!(f[F_XREAD], 0);
        assert!(f[F_TX_AFTER] >= 17 && w.facts.crossed && w.facts.tx_after == 1048576 && corroborated(&f, &w.facts));
        // A refused write to another app's program, then filler: still there...
        let mut recs = vec![rec("EXEC", 5, "/apps/themes", ""), rec("MEM_WX", 5, "0x0 len=65536", ""), rec("OPEN_W", 7, "/apps/home/main", "")];
        for n in 0..40 {
            recs.push(rec("OPEN_R", 5, &std::format!("/data/themes/{}", n), ""));
        }
        let w = window(&recs);
        assert_eq!(w.features()[F_SYSW], 1);
        assert_eq!(decide(&logits(&w.features()), &w.features(), &w.facts, true), FREEZE);
        // ...and remembered when events that count push it out.
        for n in 0..16 {
            recs.push(rec("MEM_WX", 5, &std::format!("0x0 len={}", 4096 * (n + 1)), ""));
        }
        let w = window(&recs);
        assert!(w.features()[F_SYSW] == 0 && w.facts.system_write && corroborated(&w.features(), &w.facts));
        // A window full of events that count does not take plain opens in.
        let mut w = Window::new(1);
        for n in 0..16 {
            w.add(ev_of(&rec("OPEN_R", 2, &std::format!("/data/notes/{}", n), "")).unwrap(), 0);
        }
        let before = w.features();
        w.add(ev_of(&rec("OPEN_R", 0, "/data/app/own", "")).unwrap(), 0);
        assert_eq!(w.features(), before);
    }

    #[test]
    fn what_the_phone_records_is_what_is_scored() {
        // The phone keeps 40 bytes of a command line; the text path cuts the same way.
        for line in [
            "/bin/python3 /apps/averyveryverylongname/main.py /tmp/x.py",
            "/bin/python3 /apps/averyverylongnam/m.py /tmp/x.py",
            "/bin/python3 /data/x.py",
            "/apps/\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9} /tmp/x",
        ] {
            let r = rec("EXEC", 4, line, "");
            assert_eq!(Ev::from_event(&to_event(&r).unwrap()), ev_of(&r), "{}", line);
        }
        let long = std::format!("/apps/home/{}", "x".repeat(60));
        let r = rec("OPEN_W", 6, &long, "");
        assert_eq!(Ev::from_event(&to_event(&r).unwrap()), ev_of(&r));
        let host = std::format!("{}.example.test", "a".repeat(60));
        let r = rec("SOCK_CONN", 1, &std::format!("{}:443", host), &std::format!("{}:443", host));
        assert_eq!(Ev::from_event(&to_event(&r).unwrap()), ev_of(&r));
    }

    fn added(pid: u32, name: &str) -> Subject {
        Subject::derive(pid, &std::format!("/apps/{}", name), name, [pid as u8; 4], false, false)
    }
    fn events(pid: u32, recs: &[Rec], now_ns: u64) -> Vec<TelemetryEvent> {
        recs.iter().map(|r| TelemetryEvent { app_id: pid, timestamp_us: now_ns / 1000, ..to_event(r).unwrap() }).collect()
    }
    const MS: u64 = 1_000_000;

    #[test]
    fn every_window_that_changed_gets_its_turn() {
        let mut policy = ShieldPolicy::with_mode(Mode::Enforce);
        let mut scorer = Scorer::new();
        // Two busy programs started first, each with an event every turn, and the attacker.
        let subjects = [added(1, "chat"), added(2, "radio"), added(4, "maps"), added(3, "photosync")];
        let mut seen = Vec::new();
        for tick in 0..8u64 {
            let now = tick * 100 * MS;
            let mut ev = events(1, &[rec("OPEN_R", 5, "/data/chat/log", "")], now);
            ev.extend(events(2, &[rec("OPEN_R", 5, "/data/radio/list", "")], now));
            ev.extend(events(4, &[rec("OPEN_R", 5, "/data/maps/tile", "")], now));
            if tick == 0 {
                ev.extend(events(3, &exfil(4), now));
            }
            seen.extend(scorer.step(&mut policy, &ev, 0, now, &subjects));
        }
        assert_eq!(scorer.scored, 2 * 8);
        assert!(seen.iter().any(|l| l.pid == 3 && l.action == QUARANTINE && l.enforced), "{:?}", seen);
        assert!(policy.is_quarantined(800 * MS, subjects[3].key()));
    }

    #[test]
    fn the_scorer_contains_again_after_a_quarantine_ends() {
        let mut policy = ShieldPolicy::with_mode(Mode::Enforce);
        let mut scorer = Scorer::new();
        let who = [added(3, "photosync")];
        let key = who[0].key();
        let s = 1000 * MS;
        let first = scorer.step(&mut policy, &events(3, &exfil(4), s), 0, s, &who);
        assert!(matches!(first[..], [scorer::Local { pid: 3, action: QUARANTINE, enforced: true, .. }]), "{:?}", first);
        assert_eq!(policy.guards[0].until_ns, 31 * s);
        // Nothing new: nothing more is said or done, however often it is scored.
        let more = [rec("OPEN_R", 5, "/data/photosync/state", "")];
        assert!(scorer.step(&mut policy, &events(3, &more, 2 * s), 0, 2 * s, &who).is_empty());
        // The quarantine ends, and it sends 32 KiB: under the mark, nothing yet.
        assert_eq!(policy.expire_quarantines(32 * s), [3]);
        let tx = |bytes: u32| [rec("SOCK_TX", 5, &std::format!("{}B", bytes), "185.220.101.4:4444")];
        assert!(scorer.step(&mut policy, &events(3, &tx(32 * 1024), 33 * s), 0, 33 * s, &who).is_empty());
        assert!(!policy.is_quarantined(33 * s, key));
        // Another 32 KiB: the second offence, five minutes.
        let second = scorer.step(&mut policy, &events(3, &tx(32 * 1024), 34 * s), 0, 34 * s, &who);
        assert!(matches!(second[..], [scorer::Local { pid: 3, action: QUARANTINE, enforced: true, .. }]), "{:?}", second);
        assert_eq!(policy.guards[0].until_ns, 334 * s);
        // And the third, until released.
        policy.expire_quarantines(335 * s);
        let third = scorer.step(&mut policy, &events(3, &tx(128 * 1024), 336 * s), 0, 336 * s, &who);
        assert_eq!(third.len(), 1);
        assert_eq!(policy.guards[0].until_ns, shield::FOREVER);
    }

    #[test]
    fn what_monitoring_reported_is_enforced_after_the_switch() {
        let mut policy = ShieldPolicy::with_mode(Mode::Monitor);
        let mut scorer = Scorer::new();
        let who = [added(3, "photosync")];
        let s = 1000 * MS;
        let first = scorer.step(&mut policy, &events(3, &exfil(4), s), 0, s, &who);
        assert!(matches!(first[..], [scorer::Local { action: QUARANTINE, enforced: false, .. }]), "{:?}", first);
        assert!(!policy.is_quarantined(s, who[0].key()));
        // Still monitoring: not reported again.
        let more = [rec("OPEN_R", 5, "/data/photosync/state", "")];
        assert!(scorer.step(&mut policy, &events(3, &more, 2 * s), 0, 2 * s, &who).is_empty());
        // The user switches to enforcing; the app's next recorded event is enough.
        policy.set_mode(3 * s, Mode::Enforce);
        let next = scorer.step(&mut policy, &events(3, &more, 4 * s), 0, 4 * s, &who);
        assert!(matches!(next[..], [scorer::Local { action: QUARANTINE, enforced: true, .. }]), "{:?}", next);
        assert!(policy.is_quarantined(4 * s, who[0].key()));
    }

    #[test]
    fn the_replay_uses_the_phones_own_idea_of_who_a_program_is() {
        let r = |name: &str, flags: u8| Rec { name: name.into(), ..rec("EXEC", flags, "/apps/x", "") };
        // As recorded: image apps are exempt, the three core apps have their roles.
        let notes = subject(&r("notes", 1), Origin::Recorded);
        assert_eq!((notes.name(), notes.flags, notes.role), ("notes", shield::FLAG_BACKGROUND, shield::ROLE_OWNS_APP));
        assert_eq!(subject(&r("agent", 0), Origin::Recorded).role, shield::ROLE_OWNS_APP | shield::ROLE_CORE | shield::ROLE_INSTALLER);
        assert_eq!(subject(&r("home", 0), Origin::Added).role, shield::ROLE_OWNS_APP | shield::ROLE_CORE);
        // A core app's name on a program that is not the image's earns nothing.
        assert_eq!(subject(&r("settings", 4), Origin::Recorded).role, shield::ROLE_OWNS_APP);
        // --added: everything else counts as user-added.
        assert_eq!(subject(&r("notes", 0), Origin::Added).flags, shield::FLAG_NEW_HASH);
        assert_eq!(subject(&r("notes", 17), Origin::Added).flags, shield::FLAG_NEW_HASH | shield::FLAG_BACKGROUND | shield::FLAG_DEV_PUSH);
    }

    fn lines(recs: &[Rec]) -> Vec<String> {
        recs.iter().enumerate().map(|(n, r)| std::format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", 1000 + 150 * n, r.pid, r.name, r.sha8, r.kind, r.flags, r.idle_s, r.detail, r.target)).collect()
    }

    #[test]
    fn monitor_mode_never_contains_and_enforce_does() {
        let text = lines(&exfil(4));
        let monitor = replay(text.iter().map(String::as_str), Mode::Monitor, Origin::Recorded);
        assert_eq!((monitor.contained(), monitor.denied()), (0, 0));
        assert!(monitor.apps[0].warned > 0 && monitor.apps[0].shadow > 0, "{:?}", monitor.apps);
        let enforce = replay(text.iter().map(String::as_str), Mode::Enforce, Origin::Recorded);
        assert!(enforce.apps[0].quarantined && !enforce.apps[0].frozen, "{:?}", enforce.apps);
        // From the boot image (no new-hash flag): reported only, unless strict.
        let image = lines(&exfil(0));
        let r = replay(image.iter().map(String::as_str), Mode::Enforce, Origin::Recorded);
        assert_eq!(r.contained(), 0);
        assert!(r.apps[0].warned > 0 && r.apps[0].shadow > 0);
        assert_eq!(replay(image.iter().map(String::as_str), Mode::Strict, Origin::Recorded).contained(), 1);
    }

    #[test]
    fn code_overwrite_is_refused_and_frozen_offline() {
        let text = lines(&[rec("EXEC", 4, "/apps/themes", ""), rec("MEM_WX", 4, "0x0 len=65536", ""), rec("OPEN_W", 4, "/apps/home/main", ""), rec("OPEN_W", 4, "/bin/python3", "")]);
        let r = replay(text.iter().map(String::as_str), Mode::Enforce, Origin::Recorded);
        assert_eq!(r.apps[0].denied, 2, "both writes refused by the built-in rules");
        assert!(r.apps[0].frozen && r.apps[0].quarantined, "{:?}", r.apps);
        // The built-in rules hold for image apps too; only containment is spared.
        let image = lines(&[rec("EXEC", 0, "/apps/themes", ""), rec("OPEN_W", 0, "/apps/home/main", "")]);
        let r = replay(image.iter().map(String::as_str), Mode::Enforce, Origin::Recorded);
        assert_eq!((r.apps[0].denied, r.contained()), (1, 0));
    }

    const BENIGN: &[(&str, &str)] = &[
        ("qemu-home-1", include_str!("../../targets/qemu/shield-traces/benign/qemu-home-1.log")),
        ("qemu-notes-1", include_str!("../../targets/qemu/shield-traces/benign/qemu-notes-1.log")),
        ("qemu-weather-1", include_str!("../../targets/qemu/shield-traces/benign/qemu-weather-1.log")),
        ("moto-notes-1", include_str!("../../targets/qemu/shield-traces/benign/moto-notes-1.log")),
    ];
    const PHOTOSYNC: &[&str] = &[
        include_str!("../../targets/qemu/shield-traces/attack/qemu-photosync-1.log"),
        include_str!("../../targets/qemu/shield-traces/attack/moto-photosync-1.log"),
    ];
    const THEMES: &[&str] = &[
        include_str!("../../targets/qemu/shield-traces/attack/qemu-themes-1.log"),
        include_str!("../../targets/qemu/shield-traces/attack/moto-themes-1.log"),
    ];

    /// The false-positive budget for the recorded benign traces: nothing is
    /// contained, refused or even warned about, in any mode, whether the
    /// programs count as part of the image or as added by the user.
    #[test]
    fn benign_traces_have_no_false_positives() {
        for mode in [Mode::Monitor, Mode::Enforce, Mode::Strict] {
            for origin in [Origin::Recorded, Origin::Added] {
                for (name, text) in BENIGN {
                    let r = replay(text.lines(), mode, origin);
                    assert_eq!(r.apps.len(), 1, "{}", name);
                    let a = &r.apps[0];
                    assert!(a.records >= 3 && r.scored > 0, "{} was not replayed", name);
                    assert_eq!((a.contained(), a.denied, a.warned, a.shadow), (false, 0, 0, 0), "{} in {:?}/{:?}: {:?}", name, mode, origin, a);
                }
            }
        }
    }

    #[test]
    fn recorded_attacks_are_contained_or_reported() {
        for text in PHOTOSYNC {
            // As recorded it is part of the demo image: reported, not contained.
            let r = replay(text.lines(), Mode::Enforce, Origin::Recorded);
            assert_eq!(r.contained(), 0);
            assert!(r.apps[0].warned > 0, "{:?}", r.apps);
            // Added by the user, or on a strict phone: quarantined, with no companion.
            for (mode, origin) in [(Mode::Enforce, Origin::Added), (Mode::Strict, Origin::Recorded)] {
                let r = replay(text.lines(), mode, origin);
                assert!(r.apps[0].quarantined, "{:?}/{:?}: {:?}", mode, origin, r.apps);
            }
            let r = replay(text.lines(), Mode::Monitor, Origin::Added);
            assert_eq!(r.contained(), 0);
            assert!(r.apps[0].shadow > 0, "monitoring reports what it would have done: {:?}", r.apps);
        }
        // The recorded themes windows end at the W+X map, before its write to
        // /apps/home: one sign only, so a warning (see code_overwrite_is_refused_and_frozen_offline).
        for text in THEMES {
            let r = replay(text.lines(), Mode::Enforce, Origin::Added);
            assert_eq!(r.contained(), 0);
            assert!(r.apps[0].warned > 0, "{:?}", r.apps);
        }
    }

    #[test]
    fn wilson_interval_is_honest_about_small_samples() {
        let (low, high) = wilson(0, 4);
        assert!(low == 0.0 && high > 0.48 && high < 0.50, "{}", high);
        let (low, high) = wilson(0, 1000);
        assert!(low == 0.0 && high < 0.004);
        let (low, high) = wilson(5, 100);
        assert!(low > 0.02 && low < 0.022 && high > 0.11 && high < 0.113, "{} {}", low, high);
        assert_eq!(wilson(0, 0), (0.0, 1.0));
    }
}
