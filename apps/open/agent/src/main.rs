//! Agent: talk to the paired Mac (Violet Agent, companion/mac), which listens,
//! answers and builds apps with Codex; the apps it builds are installed here
//! and stay after a restart. An unpaired phone shows a one-time QR code for
//! the Mac's camera, or reads the Mac's code with its own camera. The two
//! meet through a relay (a Chariot) from any network, switch to a direct UDP
//! path when the networks allow one, and encrypt end to end:
//! docs/agent-protocol.md.
mod direct;
mod link;
mod locomo;
mod scan;
mod secure;
mod voice;

use std::io::{BufRead, BufReader, Read};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use secure::{Identity, Offer, Session};
use serde_json::{json, Value};
use violet::{colors::*, mix, rgb, Canvas, Fonts, Rect, Screen, W};

const SAVED: &str = "/data/agent";
/// The relay unless the shared setting "relay" names another.
const RELAY: &str = match option_env!("VIOLET_AGENT_RELAY") {
    Some(r) => r,
    None => "https://agent-2af29451c05342fcbb9db79b4df48ce1.apps.chariots.sh",
};
const ORB: (f32, f32) = (360.0, 600.0);
const ORB_BOX: Rect = Rect::new(150, 390, 570, 810);
const OPEN_BOX: Rect = Rect::new(60, 1330, 660, 1440);
const UNPAIR_BOX: Rect = Rect::new(440, 1480, 680, 1560);
const SCAN_BOX: Rect = Rect::new(110, 1210, 610, 1300);
const LOCAL_BOX: Rect = Rect::new(110, 1350, 610, 1440);
const LOCAL_RELAY: &str = "http://agent-relay.violet";
const PREVIEW: Rect = Rect::new(110, 300, 610, 1180);
/// STUN servers for the direct path, unless the shared setting "stun" names
/// one (HOST:PORT, or "off" for the relay only).
const STUN: [&str; 2] = ["stun.l.google.com:19302", "stun.cloudflare.com:3478"];
/// A build's own STUN server instead (tests: the local relay's).
const STUN_BUILT: Option<&str> = option_env!("VIOLET_AGENT_STUN");
const TALK: Rect = Rect::new(0, 380, W as i32, 900);
const LINES: Rect = Rect::new(0, 900, W as i32, 1320);

enum Msg {
    Ui(violet::Event),
    Net(u32, link::Net),
    Mic(voice::Mic),
    Played(voice::Played),
    Direct(u32, direct::Event),
    Scan(scan::Scan),
    Local(Result<secure::Invitation, String>),
}

/// What the phone remembers: its identity and the Mac it trusts.
struct Saved {
    seed: [u8; 32],
    peer: Option<[u8; 32]>,
    peer_name: String,
    /// What the user calls the agent (the Mac's setting; "Violet" until it says).
    name: String,
    /// The relay the paired Mac named in the code it showed (else our own).
    relay: Option<String>,
}

impl Saved {
    fn load() -> Saved {
        let text = std::fs::read_to_string(SAVED).unwrap_or_default();
        let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_string()));
        let key = |k: &str| get(k).and_then(|h| secure::unhex(&h)).and_then(|v| <[u8; 32]>::try_from(v).ok());
        let mut s = Saved {
            seed: key("seed ").unwrap_or([0; 32]),
            peer: key("peer "),
            peer_name: get("peer_name ").unwrap_or_default(),
            name: get("name ").filter(|n| !n.is_empty()).unwrap_or_else(|| "Violet".into()),
            relay: get("relay ").filter(|r| r.contains("://")),
        };
        if s.seed == [0; 32] {
            s.seed = secure::random();
            s.save();
        }
        s
    }
    fn save(&self) {
        let mut t = format!("seed {}\nname {}\n", secure::hex(&self.seed), self.name.replace('\n', " "));
        if let Some(p) = self.peer {
            t += &format!("peer {}\npeer_name {}\n", secure::hex(&p), self.peer_name.replace('\n', " "));
            if let Some(r) = &self.relay {
                t += &format!("relay {}\n", r.replace('\n', ""));
            }
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
    /// Bytes on the wire (compressed with raw DEFLATE when `deflate`).
    size: usize,
    /// The program's length and SHA-256 once inflated.
    bytes: usize,
    sha256: String,
    deflate: bool,
    data: Vec<u8>,
    started: Instant,
}

struct Agent {
    emulator: bool,
    local_pending: bool,
    shield_requested: bool,
    shield_polled: Option<Instant>,
    me: Identity,
    saved: Saved,
    /// Our relay: the shared setting "relay", else the built-in one.
    relay: String,
    /// The setting names the relay, overriding any a code named.
    relay_set: bool,
    /// The relay in use now.
    using: String,
    /// A Mac's code the camera read: pairing with it.
    invitation: Option<secure::Invitation>,
    /// Scanning: the camera thread's stop flag, and the last frame.
    scanning: Option<Arc<AtomicBool>>,
    preview: Option<(usize, usize, Vec<u8>)>,
    /// The direct path: its thread, generation and key, where it is open,
    /// and when to try again.
    direct: Option<Sender<direct::Cmd>>,
    direct_gen: u32,
    direct_key: [u8; 32],
    direct_up: Option<String>,
    direct_next: Option<Instant>,
    direct_fails: u32,
    stun: Vec<String>,
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
    locomo: locomo::Locomo,
    /// The newest installed app: (name, title).
    built: Option<(String, String)>,
    listening: Arc<AtomicBool>,
    muted: bool,
    focused: bool,
    speech: Sender<voice::Speech>,
    speaking: bool,
    tx: Sender<Msg>,
    dirty: bool,
    mic_bytes: u64,
    gate: voice::Gate,
    offered: Instant,
    silence: u32,
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
        self.locomo.set_available(false);
        self.generation += 1;
        self.session = None;
        self.shield_requested = false;
        self.shield_polled = None;
        self.offer = None;
        self.relay_up = false;
        self.stop_direct();
        self.using = self.relay.clone();
        match (self.saved.peer, &self.invitation) {
            (Some(mac), _) => {
                self.invitation = None;
                self.secret = None;
                self.room = secure::session_room(&self.me.public(), &mac);
                self.status = format!("Looking for {}…", self.mac_name());
                if let Some(r) = self.saved.relay.clone().filter(|r| !self.relay_set || r == LOCAL_RELAY) {
                    self.using = r;
                }
            }
            (None, Some(inv)) => {
                // The Mac showed the code: its secret opens the room, on its relay.
                self.secret = Some(inv.secret);
                self.room = secure::pairing_room(&inv.secret);
                self.status = format!("Pairing with {}…", inv.name);
                if !self.relay_set || inv.relay == LOCAL_RELAY {
                    self.using = inv.relay.clone();
                }
            }
            (None, None) => {
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
        self.link = Some(link::spawn(self.using.clone(), self.room, "phone", tx));
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

    /// A message to the Mac: over the direct path when it is open, else the relay.
    fn send_raw(&self, m: Vec<u8>) {
        if let (Some(d), Some(_)) = (&self.direct, &self.direct_up) {
            let _ = d.send(direct::Cmd::Send(m));
        } else if let Some(l) = &self.link {
            let _ = l.send(m);
        }
    }

    fn relay_send(&self, m: Vec<u8>) {
        if let Some(l) = &self.link {
            let _ = l.send(m);
        }
    }

    /// Look for a direct path: a new key, our candidates to the Mac, then probes.
    fn start_direct(&mut self) {
        if self.using == LOCAL_RELAY {
            return;
        }
        self.stop_direct();
        if self.stun.is_empty() || self.session.is_none() {
            return;
        }
        self.direct_gen += 1;
        self.direct_key = secure::random();
        let (tx, rx) = mpsc::channel();
        let (gen, app) = (self.direct_gen, self.tx.clone());
        std::thread::spawn(move || {
            for e in rx {
                if app.send(Msg::Direct(gen, e)).is_err() {
                    return;
                }
            }
        });
        self.direct = Some(direct::spawn(self.direct_key, self.stun.clone(), tx));
    }

    fn stop_direct(&mut self) {
        self.direct = None;
        self.direct_up = None;
        self.direct_gen += 1;
    }

    fn on_direct(&mut self, e: direct::Event) {
        match e {
            direct::Event::Gathered(cands) => {
                let key = secure::hex(&self.direct_key);
                self.send_json(json!({"t": "direct", "key": key, "cands": cands}));
            }
            direct::Event::Open(addr) => {
                println!("agent: direct path open to {addr}; the relay stands by");
                self.direct_up = Some(addr);
                self.direct_fails = 0;
                self.status = format!("Connected to {} · direct", self.mac_name());
            }
            direct::Event::Message(m) => self.on_net(link::Net::Message(m)),
            direct::Event::Down(why, unsent) => {
                let was_up = self.direct_up.take().is_some();
                self.direct = None;
                if !why.is_empty() {
                    println!("agent: direct path {}: {why}; using the relay", if was_up { "lost" } else { "not found" });
                    if self.session.is_some() {
                        self.send_json(json!({"t": "direct", "down": why}));
                        self.status = format!("Connected to {} · relay", self.mac_name());
                    }
                    // Try again later: 30 s, doubling to 10 minutes.
                    self.direct_fails += 1;
                    let wait = 30u64 << self.direct_fails.min(5).saturating_sub(1);
                    self.direct_next = Some(Instant::now() + Duration::from_secs(wait.min(600)));
                }
                for m in unsent {
                    self.relay_send(m);
                }
            }
        }
        self.dirty = true;
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
        // Pairing with a code the Mac showed: prove we read its secret.
        let proof = self.invitation.as_ref().filter(|_| !self.paired()).map(|i| i.secret);
        let o = secure::offer(&self.me, secure::PHONE, mode, &self.room, proof.as_ref());
        self.relay_send(o.message.clone());
        self.offer = Some(o);
        self.offered = Instant::now();
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
                self.stop_direct();
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
                self.stop_direct();
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
        let trusted = match (pairing, self.saved.peer, self.secret, &self.invitation) {
            // The Mac's own code named its key.
            (true, _, Some(_), Some(inv)) => h.mode == secure::PAIRING && h.ed == inv.key,
            (true, _, Some(s), None) => h.mode == secure::PAIRING && secure::proof_ok(&s, &h),
            (false, Some(mac), _, _) => h.mode == secure::SESSION && h.ed == mac,
            _ => false,
        };
        if !trusted {
            println!("agent: refused a Mac that is not ours");
            return;
        }
        // The phone leads: only a Mac's answer to the offer it has out completes
        // a handshake; answers to older offers (crossed reconnects) are ignored.
        let ours = match self.offer.take() {
            Some(o) if o.x == h.answer => o,
            other => {
                self.offer = other;
                return;
            }
        };
        let phone = self.me.public();
        let Some(session) = secure::complete(ours, &h, secure::PHONE, &self.room, &phone, &h.ed) else { return };
        if pairing {
            let scanned = self.invitation.take();
            let local = scanned.as_ref().is_some_and(|i| i.relay == LOCAL_RELAY);
            println!("agent: paired with a new Mac{}", if local { " (local companion)" } else if scanned.is_some() { " (its code, read by the camera)" } else { "" });
            self.saved.peer = Some(h.ed);
            self.saved.peer_name = scanned.as_ref().map(|i| i.name.clone()).unwrap_or_default();
            self.saved.relay = scanned.map(|i| i.relay).filter(|r| *r != self.relay);
            self.saved.save();
            self.lines.clear();
            self.lines.push((false, "Paired. Say hello!".into()));
            // Both sides move to the session room now.
            self.connect();
            return;
        }
        self.session = Some(session);
        self.shield_requested = false;
        self.shield_polled = None;
        self.status = format!("Connected to {}", self.mac_name());
        let listening = !self.muted;
        let device = if self.emulator { "VioletOS emulator" } else { "Moto G Pure" };
        self.send_json(json!({"t": "hello", "name": device, "version": 1, "listening": listening, "python": has_python()}));
        self.update_mic();
        self.direct_fails = 0;
        self.start_direct();
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
                let seg = u32::from_le_bytes(body[..4].try_into().unwrap());
                let _ = self.speech.send(voice::Speech::Pcm(seg, body[4..].to_vec()));
            }
            0x33 => self.locomo.part(body),
            0x32 if self.shield_requested && body.len() <= 512 => {
                violet::sys(&format!("shield-agent push {}", secure::hex(body)));
            }
            0x30 if body.len() >= 4 => {
                let offset = u32::from_le_bytes(body[..4].try_into().unwrap()) as u64;
                if let Some(i) = self.install.as_mut() {
                    let (at, part) = (offset as usize, &body[4..]);
                    if at + part.len() <= i.size {
                        i.data[at..at + part.len()].copy_from_slice(part);
                    }
                }
            }
            _ => {}
        }
    }

    fn poll_shield(&mut self) {
        if !self.shield_requested || self.session.is_none()
            || self.shield_polled.is_some_and(|last| last.elapsed() < Duration::from_millis(250)) {
            return;
        }
        let command = if self.shield_polled.is_some() { "shield-agent pull" } else { "shield-agent start" };
        self.shield_polled = Some(Instant::now());
        if let Some(bytes) = secure::unhex(&violet::sys(command)).filter(|bytes| !bytes.is_empty()) {
            self.send(0x31, &bytes);
        }
    }

    fn on_json(&mut self, v: Value) {
        let t = v["t"].as_str().unwrap_or("");
        let text = |k: &str| v[k].as_str().unwrap_or("").to_string();
        match t {
            "locomo_available" => self.locomo.set_available(v["enabled"].as_bool().unwrap_or(true)),
            "locomo_library" => self.locomo.library(&v),
            "locomo_control_status" => {
                if let Some(message) = v["message"].as_str() { self.locomo.status(message); }
            }
            "locomo_status" | "locomo_track" | "locomo_end" => self.locomo.record(&v),
            "hello" => {
                self.locomo.set_available(v["locomo"] == true);
                if let Some(request) = self.locomo.resume() {
                    self.send_json(request);
                }
                self.shield_requested = !self.emulator && v["shield"].as_bool() == Some(true);
                if !self.shield_requested { self.shield_polled = None; }
                let name = text("name");
                if !name.is_empty() && name != self.saved.peer_name {
                    self.saved.peer_name = name;
                    self.saved.save();
                }
                let agent = text("agent_name");
                if !agent.is_empty() && agent != self.saved.name {
                    self.saved.name = agent.chars().take(24).collect();
                    self.saved.save();
                }
                self.status = format!("Connected to {}", self.mac_name());
            }
            // The Mac's candidates for the direct path, or word that it failed there.
            "direct" => {
                if v["down"].is_string() {
                    if self.direct_up.is_some() || self.direct.is_some() {
                        println!("agent: the Mac lost the direct path: {}", v["down"]);
                        self.direct = None; // its thread hands back what it holds
                        self.direct_up = None;
                        self.direct_next = Some(Instant::now() + Duration::from_secs(30));
                    }
                    return;
                }
                let peers: Vec<std::net::SocketAddrV4> = v["cands"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(|c| c.as_str()?.parse().ok()).collect());
                println!("agent: the Mac's candidates: {}", peers.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" "));
                if let Some(d) = &self.direct {
                    let _ = d.send(direct::Cmd::Punch(peers));
                }
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
            // The Mac's speaker (RoseOS's): duck, unduck, pause, resume, stop.
            "audio" => {
                let kind = v["ctl"]["type"].as_str().unwrap_or("").to_string();
                let gain = v["ctl"]["gain"].as_f64().unwrap_or(0.25) as f32;
                if kind == "stop" {
                    self.activity = Activity::Listening;
                }
                let _ = self.speech.send(voice::Speech::Control(kind, gain));
            }
            "install" => self.begin_install(&v),
            // The installed apps and the room left, for the Mac to list or free space.
            "apps" => {
                let mut apps = Vec::new();
                let mut free = 0u64;
                for line in violet::sys("installed").lines() {
                    let f: Vec<&str> = line.split('\t').collect();
                    match f.as_slice() {
                        [".free", n, ..] => free = n.parse().unwrap_or(0),
                        [name, n, title, ..] => apps.push(json!({"name": name, "bytes": n.parse::<u64>().unwrap_or(0), "title": title})),
                        _ => {}
                    }
                }
                self.send_json(json!({"t": "apps", "apps": apps, "free": free, "python": has_python()}));
            }
            "uninstall" => {
                let name = text("name");
                let r = if secure_name(&name) { violet::sys(&format!("uninstall {name}")) } else { "error bad name".into() };
                let ok = r.starts_with("ok");
                println!("agent: uninstall {name} -> {r}");
                if ok && self.built.as_ref().is_some_and(|(n, _)| *n == name) {
                    self.built = None;
                }
                let error = r.strip_prefix("error ").unwrap_or("").to_string();
                self.send_json(json!({"t": "uninstalled", "name": name, "ok": ok, "error": error}));
            }
            "install_end" => self.finish_install(),
            "open" => {
                let name = text("name");
                if secure_name(&name) {
                    self.open_app(&name);
                }
            }
            "unpair" => self.unpair(false),
            _ => {}
        }
    }

    fn open_app(&mut self, name: &str) {
        let result = violet::sys(&format!("open /apps/{name}"));
        let ok = result == "ok";
        let reason = result.strip_prefix("error ").unwrap_or(&result);
        if !ok {
            self.status = format!("Couldn’t open {name} · {reason}");
            self.dirty = true;
        }
        self.send_json(json!({"t": "opened", "name": name, "ok": ok, "error": if ok { "" } else { reason }}));
    }

    fn begin_install(&mut self, v: &Value) {
        let name = v["name"].as_str().unwrap_or("").to_string();
        let size = v["size"].as_u64().unwrap_or(0) as usize;
        if !secure_name(&name) || size == 0 || size > 8 << 20 {
            self.send_json(json!({"t": "installed", "name": name, "ok": false, "error": "bad name or size"}));
            return;
        }
        let title = v["title"].as_str().unwrap_or(&name).to_string();
        self.status = format!("Installing {title}…");
        self.activity = Activity::Building;
        let deflate = v["encoding"] == "deflate";
        let bytes = v["bytes"].as_u64().map_or(size, |b| b as usize);
        let sha256 = v["sha256"].as_str().unwrap_or("").to_string();
        self.install = Some(Install { name, title, size, bytes, sha256, deflate, data: vec![0; size], started: Instant::now() });
    }

    fn finish_install(&mut self) {
        let Some(i) = self.install.take() else { return };
        let staged = "/apps/.agent-install";
        println!("agent: install {}: {} bytes arrived in {} ms over the {}", i.name, i.size, i.started.elapsed().as_millis(), if self.direct_up.is_some() { "direct path" } else { "relay" });
        let data = if i.deflate {
            miniz_oxide::inflate::decompress_to_vec_with_limit(&i.data, 8 << 20).unwrap_or_default()
        } else {
            i.data.clone()
        };
        let digest = secure::hex(&secure::sha256(&[&data]));
        // Compressed programs are kept compressed ("VZ1", the length, the DEFLATE):
        // the Open Runtime runs the program and stores half the bytes.
        let stored = if i.deflate {
            let mut v = b"VZ1\0".to_vec();
            v.extend_from_slice(&(data.len() as u32).to_le_bytes());
            v.extend_from_slice(&i.data);
            v
        } else {
            data.clone()
        };
        let r = if data.len() != i.bytes || digest != i.sha256 {
            format!("error the program arrived damaged ({} of {} bytes)", data.len(), i.bytes)
        } else if let Err(e) = std::fs::write(staged, &stored) {
            let _ = std::fs::remove_file(staged);
            format!("error {e}")
        } else {
            let result = violet::sys(&format!("install {} {}", i.name, i.title));
            let _ = std::fs::remove_file(staged);
            result
        };
        let ok = r.starts_with("ok");
        let ms = i.started.elapsed().as_millis() as u64;
        println!("agent: install {} -> {r} in {ms} ms", i.name);
        // A package (Python) has nothing to open.
        let package = data.starts_with(b"VGZ1");
        if ok && package {
            self.status = format!("{} is installed", i.title);
        } else if ok {
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
        let _ = self.speech.send(voice::Speech::Control("stop".into(), 1.0));
        if tell {
            self.send_json(json!({"t": "stop", "at": now_ms()}));
        }
        self.activity = Activity::Listening;
    }

    fn unpair(&mut self, tell: bool) {
        self.locomo = locomo::Locomo::default();
        self.locomo.status("Pair Violet Agent with the Mac to make songs.");
        if tell {
            self.send_json(json!({"t": "unpair"}));
            std::thread::sleep(Duration::from_millis(300));
        }
        println!("agent: unpaired");
        self.saved.peer = None;
        self.saved.relay = None;
        self.saved.peer_name.clear();
        self.saved.save();
        self.lines.clear();
        self.built = None;
        self.connect();
    }

    /// Listening whenever paired and not muted, in front or not: the runtime
    /// keeps the agent's microphone on in the background (Home shows a mic).
    fn update_mic(&mut self) {
        let on = self.session.is_some() && !self.muted;
        self.listening.store(on, Ordering::Relaxed);
    }

    /// Read the Mac's code with the camera instead of showing ours.
    fn start_scan(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        scan::spawn(stop.clone(), tx);
        let app = self.tx.clone();
        std::thread::spawn(move || {
            for s in rx {
                if app.send(Msg::Scan(s)).is_err() {
                    return;
                }
            }
        });
        self.scanning = Some(stop);
        self.preview = None;
        self.status = "Point the camera at the code on your Mac".into();
    }

    fn stop_scan(&mut self) {
        if let Some(s) = self.scanning.take() {
            s.store(true, Ordering::Relaxed);
        }
        self.preview = None;
    }

    fn on_scan(&mut self, s: scan::Scan) {
        if self.scanning.is_none() {
            return;
        }
        match s {
            scan::Scan::Preview(w, h, g) => self.preview = Some((w, h, g)),
            scan::Scan::Failed(why) => self.status = why,
            scan::Scan::Code(text) => {
                self.stop_scan();
                if let Some(inv) = secure::parse_invitation(&text) {
                    println!("agent: read {}'s code; pairing through {}", inv.name, inv.relay);
                    self.invitation = Some(inv);
                    self.connect();
                }
            }
        }
        self.dirty = true;
    }

    fn pair_local(&mut self) {
        self.local_pending = true;
        self.status = "Looking for Violet Agent on this Mac…".into();
        self.dirty = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let stream = TcpStream::connect(("agent-pair.violet", 80))
                    .map_err(|_| "Open Violet Agent on this Mac and try again".to_string())?;
                let _ = stream.set_read_timeout(Some(Duration::from_secs(4)));
                let mut line = String::new();
                BufReader::new(stream).take(1024).read_line(&mut line)
                    .map_err(|_| "The Mac did not answer. Try again".to_string())?;
                secure::parse_invitation(&line)
                    .filter(|i| i.from_mac && i.relay == LOCAL_RELAY)
                    .ok_or_else(|| "The Mac is busy or already paired".to_string())
            })();
            let _ = tx.send(Msg::Local(result));
        });
    }

    fn tap(&mut self, x: i32, y: i32) {
        if violet::HOME_BOX.contains(x, y) {
            violet::home();
        } else if self.emulator && !self.paired() && !self.local_pending
            && self.scanning.is_none() && self.invitation.is_none() && LOCAL_BOX.contains(x, y)
        {
            self.pair_local();
        } else if !self.paired() && !self.local_pending && SCAN_BOX.contains(x, y) {
            if self.scanning.is_some() || self.invitation.is_some() {
                // Back to showing our own code.
                self.stop_scan();
                self.invitation = None;
                self.connect();
            } else {
                self.start_scan();
            }
        } else if self.paired() && UNPAIR_BOX.contains(x, y) {
            self.unpair(true);
        } else if self.paired() && ORB_BOX.contains(x, y) {
            if self.speaking || self.activity == Activity::Speaking {
                self.stop_speaking(true);
            } else {
                self.muted = !self.muted;
                self.update_mic();
                let listening = !self.muted;
                self.send_json(json!({"t": "hello", "listening": listening}));
            }
        } else if let Some((name, _)) = self.built.clone().filter(|_| OPEN_BOX.contains(x, y)) {
            self.open_app(&name);
        }
        self.dirty = true;
    }
}

/// Python apps need /bin/python3, which the handset gets as an installed package.
fn has_python() -> bool {
    std::path::Path::new("/bin/python3").exists()
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
    violet::header(c, fonts, &a.saved.name);
    let small = fonts.regular(26.0);
    let body = fonts.regular(30.0);
    small.draw(c, &small.fit(&a.status, W as i32 - 80), 40, 250, MUTED);
    if !a.paired() {
        let host = a.using.split("://").nth(1).unwrap_or(&a.using).trim_end_matches('/');
        let relay_line = format!("{} · {host}", if a.relay_up { "Relay connected" } else { "Connecting to the relay" });
        if a.scanning.is_some() {
            draw_preview(c, a.preview.as_ref(), PREVIEW);
            violet::button(c, &fonts.medium(30.0), SCAN_BOX, "Show my code instead", PANEL, INK);
        } else if let Some(inv) = &a.invitation {
            body.centered(c, &format!("Pairing with {}…", inv.name), 0, W as i32, 640, INK);
            small.centered(c, &small.fit(&relay_line, W as i32 - 80), 0, W as i32, 700, MUTED);
            violet::button(c, &fonts.medium(30.0), SCAN_BOX, "Show my code instead", PANEL, INK);
        } else {
            draw_qr(c, &a.pairing_url(), Rect::new(80, 300, 640, 860));
            body.centered(c, "Open Violet Agent on your Mac", 0, W as i32, 940, INK);
            body.centered(c, "and show it this code,", 0, W as i32, 985, INK);
            body.centered(c, "or scan the code your Mac shows.", 0, W as i32, 1030, INK);
            small.centered(c, &small.fit(&relay_line, W as i32 - 80), 0, W as i32, 1110, MUTED);
            violet::button(c, &fonts.bold(32.0), SCAN_BOX, "Scan the Mac's code", ACCENT, BACKGROUND);
            if a.emulator {
                let label = if a.local_pending { "Looking for this Mac…" } else { "Connect to this Mac" };
                violet::button(c, &fonts.bold(32.0), LOCAL_BOX, label, PANEL, INK);
            }
        }
        return;
    }
    draw_talk(c, fonts, a);
    draw_lines(c, fonts, a);
    if let Some((_, title)) = &a.built {
        violet::button(c, &fonts.bold(36.0), OPEN_BOX, &format!("Open {title}"), ACCENT, BACKGROUND);
    }
    violet::button(c, &fonts.medium(26.0), UNPAIR_BOX, "Unpair", PANEL, MUTED);
}

/// A grey camera frame, turned upright (the sensor lies on its side), scaled to fill `r`.
fn draw_preview(c: &mut Canvas, frame: Option<&(usize, usize, Vec<u8>)>, r: Rect) {
    let Some((w, h, g)) = frame else {
        c.fill(r, rgb(0, 0, 0));
        return;
    };
    // Upright, the picture is h wide and w tall.
    let scale = (r.width() as f32 / *h as f32).min(r.height() as f32 / *w as f32);
    let (uw, uh) = ((*h as f32 * scale) as i32, (*w as f32 * scale) as i32);
    let (ox, oy) = (r.x0 + (r.width() - uw) / 2, r.y0 + (r.height() - uh) / 2);
    c.fill(r, rgb(0, 0, 0));
    let width = c.w;
    for y in 0..uh {
        let sx = ((y as f32 / scale) as usize).min(w - 1);
        let row = (oy + y) as usize * width;
        for x in 0..uw {
            let sy = h - 1 - ((x as f32 / scale) as usize).min(h - 1);
            let v = g[sy * w + sx] as u32;
            c.px[row + (ox + x) as usize] = 0xff00_0000 | v << 16 | v << 8 | v;
        }
    }
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
    voice::speaker(speech_rx, played_tx);
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
    // The shared setting "relay" names another relay: a URL, or HOST:PORT for plain HTTP
    // on a local network (`build/moto-package "open-run=@sys set relay 192.168.1.20:8787"`).
    let setting = violet::sys("get relay");
    let relay_set = !matches!(setting.as_str(), "" | "error" | "default");
    let relay = match setting.as_str() {
        "" | "error" | "default" => RELAY.to_string(),
        s if s.contains("://") => s.to_string(),
        s => format!("http://{s}"),
    };
    // The direct path's STUN servers: `@sys set stun HOST:PORT`, or `off`.
    let stun: Vec<String> = match violet::sys("get stun").trim() {
        "" | "error" | "default" => match STUN_BUILT {
            Some(s) => vec![s.to_string()],
            None => STUN.iter().map(|s| s.to_string()).collect(),
        },
        "off" => Vec::new(),
        s => vec![s.to_string()],
    };
    let saved = Saved::load();
    let me = Identity::from_seed(saved.seed);
    println!("agent: key {} relay {relay}", &secure::hex(&me.public())[..16]);
    let status = violet::sys("status");
    let status = violet::fields(&status);
    let mut a = Agent {
        emulator: status.get("emulator").is_some_and(|v| v == "1"),
        local_pending: false,
        shield_requested: false,
        shield_polled: None,
        me,
        saved,
        using: relay.clone(),
        relay,
        relay_set,
        invitation: None,
        scanning: None,
        preview: None,
        direct: None,
        direct_gen: 0,
        direct_key: [0; 32],
        direct_up: None,
        direct_next: None,
        direct_fails: 0,
        stun,
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
        locomo: locomo::Locomo::default(),
        built: None,
        listening,
        muted: false,
        // Started behind Home at boot when paired, so not necessarily in front.
        focused: status.get("focused").map_or(true, |v| v == "1"),
        speech: speech_tx,
        speaking: false,
        tx,
        dirty: true,
        mic_bytes: 0,
        gate: voice::Gate::default(),
        offered: Instant::now(),
        silence: 0,
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
            Ok(Msg::Direct(gen, e)) if gen == a.direct_gen => a.on_direct(e),
            // An old path's leftovers still go through the relay.
            Ok(Msg::Direct(_, direct::Event::Down(_, unsent))) => {
                for m in unsent {
                    a.relay_send(m);
                }
            }
            Ok(Msg::Direct(..)) => {}
            Ok(Msg::Scan(s)) => a.on_scan(s),
            Ok(Msg::Local(result)) => {
                a.local_pending = false;
                if !a.paired() {
                    match result {
                        Ok(invitation) => { a.invitation = Some(invitation); a.connect(); }
                        Err(why) => { a.status = why; a.dirty = true; }
                    }
                }
            }
            Ok(Msg::Mic(voice::Mic::Samples(pcm, peak))) => {
                if a.session.is_some() && a.listening.load(Ordering::Relaxed) {
                    match a.gate.feed(pcm, peak) {
                        voice::Gated::Audio(chunks) => {
                            if a.silence > 0 {
                                let n = std::mem::take(&mut a.silence);
                                a.send(0x23, &n.to_le_bytes());
                            }
                            for c in chunks {
                                let enc = voice::mulaw(&c);
                                a.mic_bytes += enc.len() as u64;
                                a.send(0x22, &enc);
                            }
                        }
                        voice::Gated::Silence(0) => {}
                        // Silence goes as one count every 200 ms.
                        voice::Gated::Silence(n) => {
                            a.silence += n;
                            if a.silence >= 3200 {
                                let n = std::mem::take(&mut a.silence);
                                a.send(0x23, &n.to_le_bytes());
                            }
                        }
                    }
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
            Ok(Msg::Played(voice::Played::Report(r))) => {
                if let Ok(msg) = serde_json::from_str::<Value>(&r) {
                    a.send_json(json!({"t": "audio_report", "msg": msg}));
                }
            }
            Ok(Msg::Played(voice::Played::Audible(on))) => {
                a.speaking = on;
                if on {
                    a.activity = Activity::Speaking;
                } else if a.activity == Activity::Speaking {
                    a.activity = Activity::Listening;
                }
                a.dirty = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
        a.poll_shield();
        if a.session.is_none() {
            a.locomo.set_available(false);
        }
        for request in a.locomo.requests() {
            a.send_json(request);
        }
        if a.session.is_some() && a.direct.is_none() && a.direct_next.is_some_and(|t| Instant::now() > t) {
            a.direct_next = None;
            a.start_direct();
        }
        // No session while the relay is up: offer again every 4 s (a message lost on the way).
        if a.session.is_none() && a.relay_up && a.offered.elapsed() > Duration::from_secs(4) {
            a.offer_hello();
        }
        if a.since.elapsed() > Duration::from_secs(20) {
            println!("agent: microphone {} B/s to the Mac; loudest {}, floor {:.0}", a.mic_bytes / a.since.elapsed().as_secs().max(1), a.gate.loudest, a.gate.floor);
            a.gate.loudest = 0;
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

#[cfg(test)]
mod tests {
    /// The pairing screen's code, drawn as on the phone, for the Mac's decoder
    /// (companion/mac/Tests/decode_qr.swift reads it back).
    #[test]
    fn pairing_code_draws() {
        let mut c = violet::Canvas::new(720, 1600);
        let url = "violet-agent:1?r=https://example.apps.chariots.sh&k=nif642QP8M59daxr5YLqmjdgUWfZVPTYZFsd-zX23_Y&s=Cgs7s24Ubm5GCft4oiKaZJgeb61R67Vkao0TtA2kv0U&n=Moto";
        super::draw_qr(&mut c, url, violet::Rect::new(80, 330, 640, 890));
        let mut ppm = b"P6\n720 1600\n255\n".to_vec();
        for y in 0..1600 {
            for x in 0..720 {
                let p = c.get(x, y);
                ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
            }
        }
        let out = std::env::var("AGENT_QR_OUT").unwrap_or_else(|_| std::env::temp_dir().join("agent-qr.ppm").display().to_string());
        std::fs::write(out, ppm).unwrap();
    }
}
