//! Agent: talk to the paired Mac (Violet Agent, companion/mac), which listens,
//! answers and builds apps with Codex; the apps it builds are installed here
//! and stay after a restart. An unpaired phone shows a one-time QR code for
//! the Mac's camera. The two meet through a relay (a Chariot) from any
//! network and encrypt end to end: docs/agent-protocol.md.
mod link;
mod secure;
mod voice;

use std::io::{Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use secure::{Identity, Offer, Session};
use serde_json::{json, Value};
use violet::{colors::*, mix, rgb, Canvas, Fonts, Rect, Screen, W};

const SAVED: &str = "/data/agent";
/// The relay unless the shared setting agent.relay names another
/// (`set agent.relay http://192.168.1.20:8787` in /dev/violet/sys).
const RELAY: &str = match option_env!("VIOLET_AGENT_RELAY") {
    Some(r) => r,
    None => "https://violet-chariot.apps.chariots.sh",
};
const ORB: (f32, f32) = (360.0, 600.0);
const ORB_BOX: Rect = Rect::new(150, 390, 570, 810);
const OPEN_BOX: Rect = Rect::new(60, 1330, 660, 1440);
const UNPAIR_BOX: Rect = Rect::new(440, 1480, 680, 1560);
const TALK: Rect = Rect::new(0, 380, W as i32, 900);
const LINES: Rect = Rect::new(0, 900, W as i32, 1320);

enum Msg {
    Ui(violet::Event),
    Net(u32, link::Net),
    Mic(voice::Mic),
    Played(voice::Played),
}

/// What the phone remembers: its identity and the Mac it trusts.
struct Saved {
    seed: [u8; 32],
    peer: Option<[u8; 32]>,
    peer_name: String,
}

impl Saved {
    fn load() -> Saved {
        let text = std::fs::read_to_string(SAVED).unwrap_or_default();
        let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_string()));
        let key = |k: &str| get(k).and_then(|h| secure::unhex(&h)).and_then(|v| <[u8; 32]>::try_from(v).ok());
        let mut s = Saved { seed: key("seed ").unwrap_or([0; 32]), peer: key("peer "), peer_name: get("peer_name ").unwrap_or_default() };
        if s.seed == [0; 32] {
            s.seed = secure::random();
            s.save();
        }
        s
    }
    fn save(&self) {
        let mut t = format!("seed {}\n", secure::hex(&self.seed));
        if let Some(p) = self.peer {
            t += &format!("peer {}\npeer_name {}\n", secure::hex(&p), self.peer_name.replace('\n', " "));
        }
        if let Err(e) = std::fs::write(SAVED, t) {
            println!("agent: could not save: {e}");
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Activity {
    Listening,
    Thinking,
    Speaking,
    Building,
}

struct Install {
    name: String,
    title: String,
    size: usize,
    sha256: String,
    file: std::fs::File,
    started: Instant,
}

struct Agent {
    me: Identity,
    saved: Saved,
    relay: String,
    /// The link's generation: events from an older link are ignored.
    generation: u32,
    link: Option<Sender<Vec<u8>>>,
    room: [u8; 16],
    /// Unpaired: the secret in the QR code.
    secret: Option<[u8; 32]>,
    offer: Option<Offer>,
    session: Option<Session>,
    relay_up: bool,
    status: String,
    activity: Activity,
    level: f32,
    lines: Vec<(bool, String)>,
    /// The agent's words so far this turn, shown as they arrive.
    partial: Option<String>,
    install: Option<Install>,
    /// The newest installed app: (name, title).
    built: Option<(String, String)>,
    listening: Arc<AtomicBool>,
    muted: bool,
    focused: bool,
    speech: Sender<voice::Speech>,
    cancelled: Arc<AtomicU32>,
    speaking: Option<u32>,
    tx: Sender<Msg>,
    dirty: bool,
    mic_bytes: u64,
    since: Instant,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl Agent {
    fn paired(&self) -> bool {
        self.saved.peer.is_some()
    }

    /// (Re)connect to the room this phone belongs in.
    fn connect(&mut self) {
        self.generation += 1;
        self.session = None;
        self.offer = None;
        self.relay_up = false;
        match self.saved.peer {
            Some(mac) => {
                self.secret = None;
                self.room = secure::session_room(&self.me.public(), &mac);
                self.status = format!("Looking for {}…", self.mac_name());
            }
            None => {
                let s = secure::random::<32>();
                self.secret = Some(s);
                self.room = secure::pairing_room(&s);
                self.status = "Scan this code with Violet Agent on your Mac".into();
                // For tests and USB setups that pair without a camera.
                println!("agent: pairing code {}", self.pairing_url());
            }
        }
        let (tx, rx) = mpsc::channel();
        let gen = self.generation;
        let app = self.tx.clone();
        std::thread::spawn(move || {
            for e in rx {
                if app.send(Msg::Net(gen, e)).is_err() {
                    return;
                }
            }
        });
        self.link = Some(link::spawn(self.relay.clone(), self.room, "phone", tx));
        self.update_mic();
        self.dirty = true;
    }

    fn mac_name(&self) -> String {
        if self.saved.peer_name.is_empty() { "your Mac".into() } else { self.saved.peer_name.clone() }
    }

    fn pairing_url(&self) -> String {
        let s = self.secret.unwrap_or_default();
        format!("violet-agent:1?r={}&k={}&s={}&n=Moto", self.relay, secure::b64url(&self.me.public()), secure::b64url(&s))
    }

    fn send_raw(&self, m: Vec<u8>) {
        if let Some(l) = &self.link {
            let _ = l.send(m);
        }
    }

    fn send(&mut self, kind: u8, body: &[u8]) {
        let Some(s) = self.session.as_mut() else { return };
        let mut plain = Vec::with_capacity(body.len() + 1);
        plain.push(kind);
        plain.extend_from_slice(body);
        let m = s.seal(&plain);
        self.send_raw(m);
    }

    fn send_json(&mut self, v: Value) {
        self.send(0x10, v.to_string().as_bytes());
    }

    fn offer_hello(&mut self) {
        let mode = if self.paired() { secure::SESSION } else { secure::PAIRING };
        let o = secure::offer(&self.me, secure::PHONE, mode, &self.room, None);
        self.send_raw(o.message.clone());
        self.offer = Some(o);
    }

    fn on_net(&mut self, e: link::Net) {
        match e {
            link::Net::Connected => {
                self.relay_up = true;
                if self.paired() {
                    self.status = format!("Waiting for {}…", self.mac_name());
                }
            }
            link::Net::PeerUp => self.offer_hello(),
            link::Net::PeerDown => {
                self.session = None;
                self.offer = None;
                if self.paired() {
                    self.status = format!("Waiting for {}…", self.mac_name());
                }
                self.update_mic();
            }
            link::Net::Lost(why) => {
                println!("agent: relay: {why}");
                self.relay_up = false;
                self.session = None;
                self.offer = None;
                self.status = "Reconnecting…".into();
                self.update_mic();
            }
            link::Net::Message(m) if m.first() == Some(&secure::HELLO) => self.on_hello(&m),
            link::Net::Message(m) => {
                let plain = self.session.as_mut().and_then(|s| s.open(&m));
                if let Some(p) = plain {
                    self.on_record(&p);
                }
            }
        }
        self.dirty = true;
    }

    fn on_hello(&mut self, m: &[u8]) {
        let Some(h) = secure::parse_hello(m, &self.room) else {
            println!("agent: a HELLO that does not verify");
            return;
        };
        if h.role != secure::MAC {
            return;
        }
        let pairing = !self.paired();
        let trusted = match (pairing, self.saved.peer, self.secret) {
            (true, _, Some(s)) => h.mode == secure::PAIRING && secure::proof_ok(&s, &h),
            (false, Some(mac), _) => h.mode == secure::SESSION && h.ed == mac,
            _ => false,
        };
        if !trusted {
            println!("agent: refused a Mac that is not ours");
            return;
        }
        let ours = match self.offer.take() {
            Some(o) => o,
            None => {
                // The Mac reconnected: answer with a fresh key.
                let mode = if pairing { secure::PAIRING } else { secure::SESSION };
                let o = secure::offer(&self.me, secure::PHONE, mode, &self.room, None);
                self.send_raw(o.message.clone());
                o
            }
        };
        let phone = self.me.public();
        let Some(session) = secure::complete(ours, &h, secure::PHONE, &self.room, &phone, &h.ed) else { return };
        if pairing {
            println!("agent: paired with a new Mac");
            self.saved.peer = Some(h.ed);
            self.saved.peer_name = String::new();
            self.saved.save();
            self.lines.clear();
            self.lines.push((false, "Paired. Say hello!".into()));
            // Both sides move to the session room now.
            self.connect();
            return;
        }
        self.session = Some(session);
        self.status = format!("Connected to {}", self.mac_name());
        let listening = !self.muted;
        self.send_json(json!({"t": "hello", "name": "Moto G Pure", "version": 1, "listening": listening}));
        self.update_mic();
    }

    fn on_record(&mut self, p: &[u8]) {
        let (kind, body) = (p[0], &p[1..]);
        match kind {
            0x10 => {
                if let Ok(v) = serde_json::from_slice::<Value>(body) {
                    self.on_json(v);
                }
            }
            0x21 if body.len() >= 4 => {
                let id = u32::from_le_bytes(body[..4].try_into().unwrap());
                let _ = self.speech.send(voice::Speech::Pcm(id, body[4..].to_vec()));
            }
            0x30 if body.len() >= 4 => {
                let offset = u32::from_le_bytes(body[..4].try_into().unwrap()) as u64;
                if let Some(i) = self.install.as_mut() {
                    let ok = i.file.seek(SeekFrom::Start(offset)).and_then(|_| i.file.write_all(&body[4..]));
                    if let Err(e) = ok {
                        println!("agent: install write: {e}");
                    }
                }
            }
            _ => {}
        }
    }

    fn on_json(&mut self, v: Value) {
        let t = v["t"].as_str().unwrap_or("");
        let text = |k: &str| v[k].as_str().unwrap_or("").to_string();
        match t {
            "hello" => {
                let name = text("name");
                if !name.is_empty() && name != self.saved.peer_name {
                    self.saved.peer_name = name;
                    self.saved.save();
                }
                self.status = format!("Connected to {}", self.mac_name());
            }
            "state" => {
                self.activity = match v["state"].as_str() {
                    Some("thinking") => Activity::Thinking,
                    Some("speaking") => Activity::Speaking,
                    Some("building") => Activity::Building,
                    _ => Activity::Listening,
                };
                if let Some(s) = v["detail"].as_str() {
                    self.status = s.to_string();
                }
            }
            "transcript" => {
                let user = v["who"] == "user";
                let words = text("text");
                if v["final"].as_bool().unwrap_or(true) {
                    if !user {
                        self.partial = None;
                    }
                    if !words.is_empty() {
                        self.lines.push((user, words));
                    }
                    let n = self.lines.len();
                    if n > 12 {
                        self.lines.drain(..n - 12);
                    }
                } else if !user {
                    self.partial = Some(words);
                }
            }
            "say_stop" => self.stop_speaking(false),
            "say_end" => {
                let id = v["id"].as_u64().unwrap_or(0) as u32;
                let _ = self.speech.send(voice::Speech::End(id));
            }
            "install" => self.begin_install(&v),
            "install_end" => self.finish_install(),
            "open" => {
                let name = text("name");
                if secure_name(&name) {
                    let r = violet::sys(&format!("open /apps/{name}"));
                    self.send_json(json!({"t": "opened", "name": name, "ok": r.starts_with("ok")}));
                }
            }
            "unpair" => self.unpair(false),
            _ => {}
        }
    }

    fn begin_install(&mut self, v: &Value) {
        let name = v["name"].as_str().unwrap_or("").to_string();
        let size = v["size"].as_u64().unwrap_or(0) as usize;
        if !secure_name(&name) || size == 0 || size > 8 << 20 {
            self.send_json(json!({"t": "installed", "name": name, "ok": false, "error": "bad name or size"}));
            return;
        }
        match std::fs::File::create(format!("/apps/{name}")) {
            Ok(file) => {
                let title = v["title"].as_str().unwrap_or(&name).to_string();
                self.status = format!("Installing {title}…");
                self.activity = Activity::Building;
                self.install = Some(Install { name, title, size, sha256: v["sha256"].as_str().unwrap_or("").to_string(), file, started: Instant::now() });
            }
            Err(e) => self.send_json(json!({"t": "installed", "name": name, "ok": false, "error": e.to_string()})),
        }
    }

    fn finish_install(&mut self) {
        let Some(i) = self.install.take() else { return };
        drop(i.file);
        let path = format!("/apps/{}", i.name);
        let data = std::fs::read(&path).unwrap_or_default();
        let digest = secure::hex(&secure::sha256(&[&data]));
        let r = if data.len() != i.size || digest != i.sha256 {
            let _ = std::fs::remove_file(&path);
            format!("error the program arrived damaged ({} of {} bytes)", data.len(), i.size)
        } else {
            violet::sys(&format!("install {} {}", i.name, i.title))
        };
        let ok = r.starts_with("ok");
        let ms = i.started.elapsed().as_millis() as u64;
        println!("agent: install {} -> {r} in {ms} ms", i.name);
        if ok {
            self.built = Some((i.name.clone(), i.title.clone()));
            self.status = format!("{} is installed", i.title);
        } else {
            self.status = format!("Could not install {}", i.title);
        }
        self.activity = Activity::Listening;
        let error = r.strip_prefix("error ").unwrap_or("").to_string();
        self.send_json(json!({"t": "installed", "name": i.name, "ok": ok, "error": error, "ms": ms}));
    }

    fn stop_speaking(&mut self, tell: bool) {
        if let Some(id) = self.speaking {
            self.cancelled.fetch_max(id, Ordering::Relaxed);
        }
        let _ = self.speech.send(voice::Speech::Stop);
        if tell {
            self.send_json(json!({"t": "stop", "at": now_ms()}));
        }
        self.activity = Activity::Listening;
    }

    fn unpair(&mut self, tell: bool) {
        if tell {
            self.send_json(json!({"t": "unpair"}));
            std::thread::sleep(Duration::from_millis(300));
        }
        println!("agent: unpaired");
        self.saved.peer = None;
        self.saved.peer_name.clear();
        self.saved.save();
        self.lines.clear();
        self.built = None;
        self.connect();
    }

    fn update_mic(&mut self) {
        let on = self.session.is_some() && self.focused && !self.muted;
        self.listening.store(on, Ordering::Relaxed);
    }

    fn tap(&mut self, x: i32, y: i32) {
        if violet::HOME_BOX.contains(x, y) {
            violet::home();
        } else if self.paired() && UNPAIR_BOX.contains(x, y) {
            self.unpair(true);
        } else if self.paired() && ORB_BOX.contains(x, y) {
            if self.speaking.is_some() || self.activity == Activity::Speaking {
                self.stop_speaking(true);
            } else {
                self.muted = !self.muted;
                self.update_mic();
                let listening = !self.muted;
                self.send_json(json!({"t": "hello", "listening": listening}));
            }
        } else if let Some((name, _)) = self.built.clone().filter(|_| OPEN_BOX.contains(x, y)) {
            violet::sys(&format!("open /apps/{name}"));
            self.send_json(json!({"t": "opened", "name": name, "ok": true}));
        }
        self.dirty = true;
    }
}

fn secure_name(n: &str) -> bool {
    (1..=23).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b)) && n != "agent"
}

fn draw_qr(c: &mut Canvas, text: &str, r: Rect) {
    let Ok(q) = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium) else { return };
    let n = q.size() + 8; // a four-module quiet zone
    let m = (r.width().min(r.height()) / n).max(1);
    let (x0, y0) = (r.x0 + (r.width() - m * n) / 2, r.y0 + (r.height() - m * n) / 2);
    c.round_rect(Rect::new(x0 - 12, y0 - 12, x0 + m * n + 12, y0 + m * n + 12), 24.0, rgb(255, 255, 255));
    for y in 0..q.size() {
        for x in 0..q.size() {
            if q.get_module(x, y) {
                let (px, py) = (x0 + (x + 4) * m, y0 + (y + 4) * m);
                c.fill(Rect::new(px, py, px + m, py + m), rgb(0, 0, 0));
            }
        }
    }
}

fn wrap(face: &violet::Face, s: &str, width: i32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        let next = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if face.width(&next) > width && !line.is_empty() {
            lines.push(std::mem::replace(&mut line, word.to_string()));
        } else {
            line = next;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn draw(c: &mut Canvas, fonts: &Fonts, a: &Agent) {
    c.fill(c.bounds(), BACKGROUND);
    violet::header(c, fonts, "Agent");
    let small = fonts.regular(26.0);
    let body = fonts.regular(30.0);
    small.draw(c, &small.fit(&a.status, W as i32 - 80), 40, 250, MUTED);
    if !a.paired() {
        draw_qr(c, &a.pairing_url(), Rect::new(80, 330, 640, 890));
        let host = a.relay.split("://").nth(1).unwrap_or(&a.relay).trim_end_matches('/');
        body.centered(c, "Open Violet Agent on your Mac", 0, W as i32, 980, INK);
        body.centered(c, "and show it this code.", 0, W as i32, 1025, INK);
        small.centered(c, &small.fit(&format!("Relay: {host}"), W as i32 - 80), 0, W as i32, 1100, MUTED);
        small.centered(c, if a.relay_up { "Relay connected" } else { "Connecting to the relay…" }, 0, W as i32, 1145, MUTED);
        return;
    }
    draw_talk(c, fonts, a);
    draw_lines(c, fonts, a);
    if let Some((_, title)) = &a.built {
        violet::button(c, &fonts.bold(36.0), OPEN_BOX, &format!("Open {title}"), ACCENT, BACKGROUND);
    }
    violet::button(c, &fonts.medium(26.0), UNPAIR_BOX, "Unpair", PANEL, MUTED);
}

fn draw_talk(c: &mut Canvas, fonts: &Fonts, a: &Agent) {
    c.fill(TALK, BACKGROUND);
    let online = a.session.is_some();
    let (color, label) = match (online, a.muted, a.activity) {
        (false, _, _) => (PANEL, "Offline"),
        (true, _, Activity::Speaking) => (ACCENT, "Speaking · tap to stop"),
        (true, _, Activity::Thinking) => (rgb(0x9d, 0x8c, 0xf0), "Thinking…"),
        (true, _, Activity::Building) => (rgb(0xf2, 0x9e, 0x38), "Building…"),
        (true, true, _) => (PANEL, "Muted · tap to listen"),
        (true, false, Activity::Listening) => (rgb(0x6e, 0xc8, 0xa8), "Listening"),
    };
    let pulse = if online && !a.muted { a.level * 60.0 } else { 0.0 };
    c.circle(ORB.0, ORB.1, 170.0 + pulse, mix(BACKGROUND, color, 0.25));
    c.circle(ORB.0, ORB.1, 140.0 + pulse * 0.5, color);
    fonts.medium(30.0).centered(c, label, 0, W as i32, 860, INK);
}

fn draw_lines(c: &mut Canvas, fonts: &Fonts, a: &Agent) {
    c.fill(LINES, BACKGROUND);
    let face = fonts.regular(30.0);
    let mut rows: Vec<(bool, String)> = Vec::new();
    let all = a.lines.iter().cloned().chain(a.partial.clone().map(|p| (false, p)));
    for (user, text) in all {
        for l in wrap(&face, &text, W as i32 - 100) {
            rows.push((user, l));
        }
    }
    let show = rows.len().saturating_sub(10);
    for (k, (user, l)) in rows[show..].iter().enumerate() {
        let y = LINES.y0 + 34 + k as i32 * 40;
        let x = if *user { 60 } else { 40 };
        face.draw(c, l, x, y, if *user { MUTED } else { INK });
    }
}

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let ui = violet::events().expect("no events");
    let fonts = Fonts::load();
    let (tx, rx) = mpsc::channel::<Msg>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for e in ui {
                if tx.send(Msg::Ui(e)).is_err() {
                    return;
                }
            }
        });
    }
    let listening = Arc::new(AtomicBool::new(false));
    let (mic_tx, mic_rx) = mpsc::channel();
    voice::microphone(listening.clone(), mic_tx);
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for m in mic_rx {
                if tx.send(Msg::Mic(m)).is_err() {
                    return;
                }
            }
        });
    }
    let (speech_tx, speech_rx) = mpsc::channel();
    let (played_tx, played_rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicU32::new(0));
    voice::speaker(speech_rx, cancelled.clone(), played_tx);
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for p in played_rx {
                if tx.send(Msg::Played(p)).is_err() {
                    return;
                }
            }
        });
    }
    let setting = violet::sys("get agent.relay");
    let relay = if setting.starts_with("http") || setting.starts_with("ws") { setting } else { RELAY.to_string() };
    let saved = Saved::load();
    let me = Identity::from_seed(saved.seed);
    println!("agent: key {} relay {relay}", &secure::hex(&me.public())[..16]);
    let mut a = Agent {
        me,
        saved,
        relay,
        generation: 0,
        link: None,
        room: [0; 16],
        secret: None,
        offer: None,
        session: None,
        relay_up: false,
        status: String::new(),
        activity: Activity::Listening,
        level: 0.0,
        lines: Vec::new(),
        partial: None,
        install: None,
        built: None,
        listening,
        muted: false,
        focused: true,
        speech: speech_tx,
        cancelled,
        speaking: None,
        tx,
        dirty: true,
        mic_bytes: 0,
        since: Instant::now(),
    };
    a.connect();
    let mut c = screen.canvas();
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    loop {
        match rx.recv_timeout(Duration::from_millis(66)) {
            Ok(Msg::Ui(e)) if e.focus_gained() => {
                a.focused = true;
                a.update_mic();
                a.dirty = true;
            }
            Ok(Msg::Ui(e)) if e.kind == violet::device::LIFECYCLE && e.value == violet::device::FOCUS_LOST => {
                a.focused = false;
                a.update_mic();
            }
            Ok(Msg::Ui(e)) if e.kind == violet::device::TOUCH_DOWN => a.tap(e.x, e.y),
            Ok(Msg::Ui(_)) => {}
            Ok(Msg::Net(gen, e)) if gen == a.generation => a.on_net(e),
            Ok(Msg::Net(..)) => {}
            Ok(Msg::Mic(voice::Mic::Samples(pcm, peak))) => {
                if a.session.is_some() && a.listening.load(Ordering::Relaxed) {
                    a.mic_bytes += pcm.len() as u64;
                    a.send(0x20, &pcm);
                }
                let level = (peak as f32 / 8000.0).min(1.0);
                if (level - a.level).abs() > 0.05 {
                    a.level = level;
                    a.dirty = true;
                }
            }
            Ok(Msg::Mic(voice::Mic::Failed(why))) => {
                println!("agent: {why}");
                a.status = why;
                a.dirty = true;
            }
            Ok(Msg::Played(voice::Played::Started(id))) => {
                a.speaking = Some(id);
            }
            Ok(Msg::Played(voice::Played::Finished(id))) => {
                if a.speaking == Some(id) {
                    a.speaking = None;
                }
                a.send_json(json!({"t": "played", "id": id}));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
        if a.since.elapsed() > Duration::from_secs(60) && a.mic_bytes > 0 {
            println!("agent: microphone {} B/s to the Mac", a.mic_bytes / a.since.elapsed().as_secs().max(1));
            a.mic_bytes = 0;
            a.since = Instant::now();
        }
        if a.dirty && a.focused && last_draw.elapsed() > Duration::from_millis(60) {
            a.dirty = false;
            last_draw = Instant::now();
            draw(&mut c, &fonts, &a);
            screen.show_all(&c);
        }
    }
}
