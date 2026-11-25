//! bitchat for VioletOS: public chat with nearby phones running bitchat
//! (iOS and Android), over Bluetooth LE with no internet in between.
//!
//! The phone advertises the bitchat service and serves its characteristic
//! (ble.rs); peers connect, subscribe and exchange signed packets (proto.rs).
//! Only the public "mesh" channel is supported: no private messages yet, and
//! no relaying (this phone is a leaf of the mesh).
mod ble;
mod keyboard;
mod proto;

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use keyboard::{Key, Keyboard};
use violet::device::{TOUCH_DOWN, TOUCH_MOVE, TOUCH_UP};
use violet::{colors::*, mix, rgb, Canvas, Fonts, Rect, Screen, W};

const SAVED: &str = "/data/bitchat";
/// Signed text must stay under iOS's compression threshold (proto::COMPRESS_FROM).
const MAX_TEXT: usize = 99;
const ANNOUNCE_EVERY: Duration = Duration::from_secs(12);
const GREEN: violet::Color = rgb(0x5c, 0xd6, 0x8a);

enum Input {
    Touch(violet::Event),
    Focus,
    /// One H4 packet from the controller.
    Hci(Vec<u8>),
    /// The controller is off (or went off).
    HciDown,
    /// The difference between the network's clock and ours, in milliseconds.
    Clock(i64),
}

struct Peer {
    nickname: String,
    signing_key: [u8; 32],
    seen: Instant,
}

struct Line {
    from: String,
    text: String,
    mine: bool,
}

struct App {
    host: ble::Host,
    me: Option<proto::Identity>,
    nickname: String,
    peers: HashMap<[u8; 8], Peer>,
    lines: Vec<Line>,
    seen: VecDeque<String>,
    assembler: proto::Assembler,
    /// Milliseconds to add to the local clock; set from HTTPS or a peer.
    offset: i64,
    clock_known: bool,
    down: bool,
    typing: bool,
    text: String,
    keyboard: Keyboard,
    pressed: Option<Hit>,
    last_announce: Instant,
    entropy: Vec<u8>,
    status: String,
    /// Addresses heard advertising bitchat.
    nearby: Vec<([u8; 6], i8)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Home,
    Composer,
    Send,
}

const COMPOSER: Rect = Rect::new(28, 1520, 552, 1584);
const SEND: Rect = Rect::new(568, 1520, 692, 1584);
const TYPING_COMPOSER: Rect = Rect::new(28, 950, 552, 1014);
const TYPING_SEND: Rect = Rect::new(568, 950, 692, 1014);

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl App {
    fn clock(&self) -> u64 {
        (now_ms() as i64 + self.offset) as u64
    }

    fn composer(&self) -> (Rect, Rect) {
        if self.typing { (TYPING_COMPOSER, TYPING_SEND) } else { (COMPOSER, SEND) }
    }
    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let (c, s) = self.composer();
        if violet::HOME_BOX.contains(x, y) {
            Some(Hit::Home)
        } else if s.contains(x, y) {
            Some(Hit::Send)
        } else if c.contains(x, y) {
            Some(Hit::Composer)
        } else {
            None
        }
    }

    /// Handles what the BLE host reported and keeps announcing.
    fn service(&mut self) {
        // Random bytes from the controller (LE Rand) seed the address and keys.
        let results: Vec<_> = self.host.results.drain(..).collect();
        for (op, status, ret) in results {
            if op == 0x2018 && status == 0 && self.me.is_none() {
                self.entropy.extend_from_slice(&ret);
                if self.entropy.len() >= 16 {
                    self.begin();
                }
            }
        }
        if let Some(f) = self.host.failure.take() {
            println!("bitchat: {f}");
            self.status = format!("Bluetooth error: {f}");
        }
        while let Some(e) = self.host.events.pop_front() {
            match e {
                ble::Event::Connected(h) => println!("bitchat: link {h:#x} connected"),
                ble::Event::Disconnected(h) => println!("bitchat: link {h:#x} closed"),
                ble::Event::Subscribed(h) => {
                    println!("bitchat: link {h:#x} subscribed");
                    self.announce_to(Some(h));
                }
                ble::Event::Data(h, bytes) => self.packet(h, &bytes),
                ble::Event::Seen(a, kind, rssi) => {
                    if !self.nearby.iter().any(|x| x.0 == a) {
                        println!("bitchat: a bitchat device {} ({kind}) at {rssi} dBm", proto::hex(&a));
                        self.nearby.push((a, rssi));
                    }
                }
            }
        }
        if self.me.is_some() && self.last_announce.elapsed() >= ANNOUNCE_EVERY {
            self.announce_to(None);
        }
        self.peers.retain(|_, p| p.seen.elapsed() < Duration::from_secs(90));
    }

    fn begin(&mut self) {
        let mut seed = self.entropy.clone();
        let mut more = [0u8; 32];
        if let Ok(mut f) = File::open("/dev/urandom") {
            let _ = f.read_exact(&mut more);
        }
        seed.extend_from_slice(&more);
        seed.extend_from_slice(&now_ms().to_le_bytes());
        let d = |tag: &[u8]| -> [u8; 32] {
            let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
            ctx.update(tag);
            ctx.update(&seed);
            ctx.finish().as_ref().try_into().unwrap()
        };
        let address: [u8; 6] = d(b"address")[..6].try_into().unwrap();
        let (signing, noise, nickname) = load().unwrap_or_else(|| {
            let (s, n) = (d(b"signing"), d(b"noise"));
            let nick = format!("violet{}", &proto::hex(&d(b"nick")[..2]));
            store(&s, &n, &nick);
            (s, n, nick)
        });
        let me = proto::Identity::new(&signing, &noise);
        println!("bitchat: I am {} ({})", nickname, proto::hex(&me.id));
        self.me = Some(me);
        self.nickname = nickname;
        self.host.advertise(address);
    }

    fn announce_to(&mut self, link: Option<u16>) {
        let Some(me) = &self.me else { return };
        let p = me.announce(&self.nickname, self.clock());
        self.last_announce = Instant::now();
        self.send(&p, link);
    }

    /// Sends a packet to one link, or to every subscribed link.
    fn send(&mut self, p: &proto::Packet, only: Option<u16>) {
        let frame = p.encode();
        for (h, limit) in self.host.subscribed() {
            if only.is_some_and(|o| o != h) {
                continue;
            }
            if frame.len() <= limit {
                self.host.notify(h, &frame);
            } else {
                let mut id = [0u8; 8];
                id.copy_from_slice(&ring::digest::digest(&ring::digest::SHA256, &frame).as_ref()[..8]);
                for f in proto::fragments(p, &frame, limit, id) {
                    self.host.notify(h, &f.encode());
                }
            }
        }
    }

    fn packet(&mut self, link: u16, bytes: &[u8]) {
        let Some(p) = proto::decode(bytes) else {
            println!("bitchat: {} bytes from {link:#x} that are not a packet", bytes.len());
            return;
        };
        let Some(me) = &self.me else { return };
        if p.sender == me.id {
            return;
        }
        // The first peer's clock sets ours if the network could not.
        if !self.clock_known && matches!(p.kind, proto::ANNOUNCE | proto::MESSAGE) {
            self.offset = p.timestamp as i64 - now_ms() as i64;
            self.clock_known = true;
            println!("bitchat: clock set from a peer (offset {} ms)", self.offset);
        }
        if p.kind == proto::FRAGMENT {
            if let Some(f) = proto::Fragment::decode(&p.payload) {
                if let Some(whole) = self.assembler.add(p.sender, f) {
                    self.handle(link, whole);
                }
            }
            return;
        }
        self.handle(link, p);
    }

    fn handle(&mut self, link: u16, p: proto::Packet) {
        let digest = ring::digest::digest(&ring::digest::SHA256, &p.payload);
        let key = format!("{}-{}-{}-{}", proto::hex(&p.sender), p.timestamp, p.kind, proto::hex(&digest.as_ref()[..4]));
        if self.seen.contains(&key) {
            return;
        }
        self.seen.push_back(key);
        if self.seen.len() > 1000 {
            self.seen.pop_front();
        }
        let from = proto::hex(&p.sender);
        match p.kind {
            proto::ANNOUNCE => {
                let Some(a) = p.content().and_then(|c| proto::Announce::decode(&c)) else { return };
                if proto::peer_id(&a.noise_key) != p.sender || !p.verify(&a.signing_key) {
                    println!("bitchat: announce from {from} does not check out");
                    return;
                }
                let new = !self.peers.contains_key(&p.sender);
                if let Some(old) = self.peers.get(&p.sender) {
                    if old.signing_key != a.signing_key {
                        println!("bitchat: {from} changed its signing key; ignored");
                        return;
                    }
                }
                println!("bitchat: {} is {from}{}", a.nickname, if new { " (new)" } else { "" });
                if new {
                    self.lines.push(Line { from: String::new(), text: format!("{} joined", a.nickname), mine: false });
                }
                self.peers.insert(p.sender, Peer { nickname: a.nickname, signing_key: a.signing_key, seen: Instant::now() });
                if new {
                    self.announce_to(Some(link));
                }
            }
            proto::MESSAGE if p.recipient.map_or(true, |r| r == proto::BROADCAST) => {
                let Some(peer) = self.peers.get_mut(&p.sender) else {
                    println!("bitchat: message from {from}, who has not announced");
                    return;
                };
                if !p.verify(&peer.signing_key) {
                    println!("bitchat: message from {from} has a bad signature");
                    return;
                }
                peer.seen = Instant::now();
                let text = String::from_utf8_lossy(&p.content().unwrap_or_default()).into_owned();
                println!("bitchat: <{}> {}", peer.nickname, text);
                self.lines.push(Line { from: peer.nickname.clone(), text, mine: false });
            }
            proto::LEAVE => {
                if let Some(peer) = self.peers.get(&p.sender) {
                    if p.verify(&peer.signing_key) {
                        let name = peer.nickname.clone();
                        self.peers.remove(&p.sender);
                        self.lines.push(Line { from: String::new(), text: format!("{name} left"), mine: false });
                    }
                }
            }
            proto::REQUEST_SYNC | proto::NOISE_HANDSHAKE | proto::NOISE_ENCRYPTED => {}
            k => println!("bitchat: ignoring packet type {k:#04x} from {from}"),
        }
    }

    fn say(&mut self) {
        let text = self.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(me) = &self.me else { return };
        let p = me.message(&text, self.clock());
        self.send(&p, None);
        println!("bitchat: <{}> {}", self.nickname, text);
        self.lines.push(Line { from: self.nickname.clone(), text, mine: true });
        self.text.clear();
    }

    fn touch(&mut self, kind: u16, x: i32, y: i32) -> bool {
        match kind {
            TOUCH_DOWN => {
                if self.typing {
                    if let Some(k) = self.keyboard.key_at(x, y) {
                        self.keyboard.pressed = Some(k);
                        return true;
                    }
                }
                self.pressed = self.hit(x, y);
            }
            TOUCH_MOVE => {
                if self.keyboard.pressed.is_some() {
                    self.keyboard.pressed = self.keyboard.key_at(x, y);
                }
            }
            TOUCH_UP => {
                if let Some(k) = self.keyboard.pressed.take() {
                    self.keyboard.press(k, &mut self.text, MAX_TEXT);
                    return true;
                }
                match self.pressed.take() {
                    Some(Hit::Home) => violet::home(),
                    Some(Hit::Composer) => self.typing = !self.typing,
                    Some(Hit::Send) => {
                        self.say();
                        self.typing = false;
                    }
                    None => self.typing = false,
                }
            }
            _ => return false,
        }
        true
    }

    fn draw(&self, c: &mut Canvas, f: &Fonts) {
        c.fill(c.bounds(), BACKGROUND);
        violet::header(c, f, "bitchat");
        let small = f.regular(26.0);
        let status = if self.down {
            "Bluetooth is off. It starts with Wi-Fi: join a network.".to_string()
        } else if !self.status.is_empty() {
            self.status.clone()
        } else if !self.host.ready {
            "Starting Bluetooth…".to_string()
        } else {
            let n = self.peers.len();
            format!(
                "{} · {} {} nearby{}",
                self.nickname,
                n,
                if n == 1 { "person" } else { "people" },
                if self.clock_known { "" } else { " · clock not set" }
            )
        };
        let dot = if self.down || !self.host.ready { MUTED } else if self.peers.is_empty() { ACCENT } else { GREEN };
        c.circle(48.0, 236.0, 9.0, dot);
        small.draw(c, &small.fit(&status, 620), 68, 246, MUTED);
        let names: Vec<&str> = self.peers.values().map(|p| p.nickname.as_str()).collect();
        if !names.is_empty() {
            small.draw(c, &small.fit(&format!("#mesh: {}", names.join(", ")), 650), 40, 290, INK);
        }
        // Messages, newest at the bottom, wrapped to the width.
        let (composer, send) = self.composer();
        let bottom = composer.y0 - 24;
        let face = f.regular(32.0);
        let bold = f.bold(32.0);
        let mut rows: Vec<(String, String, violet::Color)> = Vec::new();
        for l in &self.lines {
            let (who, ink) = if l.from.is_empty() {
                (String::new(), MUTED)
            } else {
                (format!("<{}> ", l.from), if l.mine { ACCENT } else { GREEN })
            };
            let mut first = true;
            let mut line = String::new();
            let indent = bold.width(&who);
            for word in l.text.split(' ') {
                let room = 650 - if first { indent } else { 0 };
                let tried = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
                if face.width(&tried) > room && !line.is_empty() {
                    rows.push((if first { who.clone() } else { String::new() }, std::mem::take(&mut line), ink));
                    first = false;
                    line = word.to_string();
                } else {
                    line = tried;
                }
            }
            rows.push((if first { who } else { String::new() }, line, ink));
        }
        let mut y = bottom;
        c.set_clip(Rect::new(0, 320, W as i32, bottom + 8));
        for (who, text, ink) in rows.iter().rev() {
            if y < 330 {
                break;
            }
            let x = if who.is_empty() { 40 } else { 40 + bold.draw(c, who, 40, y, *ink) - 40 };
            face.draw(c, text, x, y, if ink == &MUTED { MUTED } else { INK });
            y -= 44;
        }
        c.reset_clip();
        if self.lines.is_empty() {
            let hint = if self.peers.is_empty() {
                "Open bitchat on a phone nearby."
            } else {
                "Say hello to the mesh."
            };
            f.regular(30.0).centered(c, hint, 0, W as i32, 760, MUTED);
        }
        // The composer.
        let lit = |h: Hit| self.pressed == Some(h);
        c.round_rect(composer, 22.0, if lit(Hit::Composer) { mix(PANEL, INK, 0.2) } else { PANEL });
        let shown = if self.text.is_empty() && !self.typing { "Message #mesh".to_string() } else { format!("{}{}", self.text, if self.typing { "|" } else { "" }) };
        let ink = if self.text.is_empty() && !self.typing { MUTED } else { INK };
        let tf = f.regular(30.0);
        let fitted = if tf.width(&shown) > composer.width() - 40 {
            // Show the end of a long message.
            let chars: Vec<char> = shown.chars().collect();
            let mut k = 0;
            while k < chars.len() && tf.width(&chars[k..].iter().collect::<String>()) > composer.width() - 40 {
                k += 1;
            }
            chars[k..].iter().collect()
        } else {
            shown
        };
        tf.draw(c, &fitted, composer.x0 + 20, composer.y0 + 43, ink);
        let can_send = self.host.ready && !self.text.trim().is_empty() && self.me.is_some();
        violet::button(c, &f.medium(28.0), send, "Send", if lit(Hit::Send) || can_send { ACCENT } else { PANEL }, if can_send { BACKGROUND } else { MUTED });
        if self.typing {
            small.draw(c, &format!("{}/{MAX_TEXT}", self.text.len()), 40, composer.y1 + 18, MUTED);
            self.keyboard.draw(c, f);
        }
    }
}

fn load() -> Option<([u8; 32], [u8; 32], String)> {
    let text = std::fs::read_to_string(SAVED).ok()?;
    let mut w = text.split_whitespace();
    let mut key = || -> Option<[u8; 32]> {
        let h = w.next()?;
        let v: Vec<u8> = (0..h.len()).step_by(2).filter_map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok()).collect();
        v.try_into().ok()
    };
    let (s, n) = (key()?, key()?);
    let nick = w.next()?.to_string();
    Some((s, n, nick))
}
fn store(s: &[u8; 32], n: &[u8; 32], nick: &str) {
    if let Err(e) = std::fs::write(SAVED, format!("{} {} {}\n", proto::hex(s), proto::hex(n), nick)) {
        println!("bitchat: could not save the identity: {e}");
    }
}

/// The network's time, from an HTTPS server's Date header ("Wed, 25 Nov 2026 18:04:05 GMT").
fn network_clock() -> Option<i64> {
    let before = now_ms();
    let r = violet::http::get("https://www.google.com/generate_204", &[]).ok()?;
    let date = r.header("date")?;
    let p: Vec<&str> = date.split_whitespace().collect();
    let (day, month, year, time) = (p.get(1)?.parse::<i64>().ok()?, *p.get(2)?, p.get(3)?.parse::<i64>().ok()?, p.get(4)?);
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| *m == month)? as i64 + 1;
    let t: Vec<i64> = time.split(':').filter_map(|x| x.parse().ok()).collect();
    if t.len() != 3 {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let (y, m) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + t[0] * 3600 + t[1] * 60 + t[2];
    // The server's second began somewhere in the request: take its middle.
    let local = (before + now_ms()) / 2;
    Some(secs * 1000 + 500 - local as i64)
}

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let fonts = Fonts::load();
    let mut c = screen.canvas();
    let (tx, rx) = mpsc::channel();

    let touches = violet::events().expect("no events");
    let t = tx.clone();
    std::thread::spawn(move || {
        while let Ok(e) = touches.recv() {
            let input = if e.focus_gained() { Input::Focus } else { Input::Touch(e) };
            if t.send(input).is_err() {
                return;
            }
        }
    });
    let t = tx.clone();
    std::thread::spawn(move || {
        for _ in 0..20 {
            if let Some(offset) = network_clock() {
                let _ = t.send(Input::Clock(offset));
                return;
            }
            std::thread::sleep(Duration::from_secs(15));
        }
    });

    let hci = match OpenOptions::new().read(true).write(true).open("/dev/violet/hci") {
        Ok(f) => std::sync::Arc::new(f),
        Err(e) => {
            println!("bitchat: no Bluetooth controller: {e}");
            return;
        }
    };
    let reader = hci.clone();
    let t = tx.clone();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 1100];
        let mut was_down = false;
        loop {
            match (&*reader).read(&mut buf) {
                Ok(n) if n > 0 => {
                    was_down = false;
                    if t.send(Input::Hci(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                _ => {
                    if !was_down && t.send(Input::HciDown).is_err() {
                        return;
                    }
                    was_down = true;
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    });

    let mut app = App {
        host: ble::Host::new("VioletOS"),
        me: None,
        nickname: String::new(),
        peers: HashMap::new(),
        lines: Vec::new(),
        seen: VecDeque::new(),
        assembler: proto::Assembler::default(),
        offset: 0,
        clock_known: false,
        down: false,
        typing: false,
        text: String::new(),
        keyboard: Keyboard::default(),
        pressed: None,
        last_announce: Instant::now(),
        entropy: Vec::new(),
        status: String::new(),
        nearby: Vec::new(),
    };
    println!("bitchat: starting");
    app.host.start();
    let mut dirty = true;
    let mut retried = Instant::now();
    loop {
        // Everything the host wants to send, one packet per write.
        while let Some(p) = app.host.out.pop_front() {
            if let Err(e) = (&*hci).write_all(&p) {
                if !app.down {
                    println!("bitchat: the controller is off ({e})");
                }
                app.down = true;
                app.host.out.clear();
                dirty = true;
                break;
            }
        }
        if dirty {
            app.draw(&mut c, &fonts);
            screen.show_all(&c);
            dirty = false;
        }
        let mut next = match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(i) => Some(i),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let peers = app.peers.len();
        while let Some(input) = next {
            match input {
                Input::Touch(e) => dirty |= app.touch(e.kind, e.x, e.y),
                Input::Focus => dirty = true,
                Input::Hci(p) => {
                    if app.down {
                        println!("bitchat: the controller is on");
                        app.down = false;
                    }
                    app.host.receive(&p);
                    dirty = true;
                }
                Input::HciDown => {
                    if !app.down {
                        println!("bitchat: the controller is off");
                    }
                    app.down = true;
                    dirty = true;
                }
                Input::Clock(offset) => {
                    println!("bitchat: clock set from the network (offset {offset} ms)");
                    app.offset = offset;
                    app.clock_known = true;
                    dirty = true;
                }
            }
            next = rx.try_recv().ok();
        }
        if app.down && retried.elapsed() > Duration::from_secs(3) {
            // Start over now and then: Radio may have the chip running by now
            // (or it restarted the controller, which forgets everything).
            retried = Instant::now();
            app.host = ble::Host::new("VioletOS");
            app.me = None;
            app.entropy.clear();
            app.peers.clear();
            app.host.start();
        }
        app.service();
        dirty |= app.peers.len() != peers;
    }
}
