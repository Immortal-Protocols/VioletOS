//! Settings › Updates: download a signed system update over HTTPS and stage
//! it in the phone's other boot slot.
//!
//! The bundle (tools/ota_bundle.py) is a 512-byte signed manifest and the
//! system image. Settings sends the manifest first and the supervisor checks
//! it (signature, this phone's target, not older than the running system)
//! before any of the image is downloaded; then each 64 KiB goes to the
//! inactive slot through the Open Runtime's `update` commands
//! (runtime/open/update.rs), Storage reads it all back and hashes it, and
//! the supervisor switches the slot only if that read-back digest is the
//! signed one. Settings itself decides nothing: a bundle it got wrong is
//! refused. The next start tries the new system; it is kept once it has run
//! healthy, and otherwise the phone goes back to this one by itself.
//!
//! The server is set when the image is built (VIOLET_UPDATE_URL); a build
//! without one says so.
use std::sync::{Arc, Mutex};

use violet::device::{TOUCH_DOWN, TOUCH_UP};
use violet::paper::{slab, ALERT, CREAM, GOOD, MUTED, PAPER, PAPER_PRESSED, PAPER_RAISED, VIOLET};
use violet::{Canvas, Fonts, Rect, HOME_BOX, W};

pub const URL: Option<&str> = option_env!("VIOLET_UPDATE_URL");
const PIECE: usize = 64 << 10;
const HASH_STEP: u32 = 2048;
/// Settings' own file, which no other app can write or read (runtime/open/update.rs `staging`).
const STAGING: &str = "/data/settings.ota-staging";
pub const INSTALL: Rect = Rect::new(40, 900, W as i32 - 40, 1010);

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Idle,
    Checking,
    Downloading { done: u64, total: u64 },
    Verifying { done: u32, total: u32 },
    /// Staged: the next start runs this version.
    Ready(u32),
    Failed(String),
}

/// What the supervisor's refusal codes mean (supervisor.c, ota_request).
pub fn refusal(code: u32) -> &'static str {
    match code {
        1 => "The update is damaged",
        2 => "The update is for another kind of phone",
        3 => "The update is not signed by a key this phone trusts",
        4 => "The update's signature is not valid",
        5 => "The update is older than this system",
        6 => "The update does not fit this phone",
        7 => "The downloaded system does not match its signature",
        8 => "Storage could not stage the update",
        10 => "Finish starting this system first: it can be updated once it has run without trouble for a moment",
        _ => "The phone could not switch to the update",
    }
}

/// Where the bundle comes from: the download, or a test's bytes.
pub trait Source {
    fn length(&self) -> u64;
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
}
impl Source for crate::fetch::Download {
    fn length(&self) -> u64 {
        self.length
    }
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        crate::fetch::Download::read(self, buf)
    }
}

fn field(answer: &str, key: &str) -> Option<u32> {
    violet::fields(answer).get(key).and_then(|v| v.parse().ok())
}

/// The whole install: `sys` is /dev/violet/sys, `stage` writes the next piece
/// where the runtime takes it from. Returns the staged version.
pub fn install(src: &mut dyn Source, sys: &mut dyn FnMut(&str) -> String, stage: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
               progress: &mut dyn FnMut(Phase)) -> Result<u32, String> {
    let info = sys("update info");
    let slot = field(&info, "slot").unwrap_or(0);
    if slot < 2 {
        return Err("This phone has no second system slot".into());
    }
    // While this system is itself a trial, the other slot is the only one
    // known to start: Storage refuses to touch it.
    if field(&info, "trial") != Some(0) {
        return Err(refusal(10).into());
    }
    let mut manifest = [0u8; 512];
    if src.length() < 1024 || src.read(&mut manifest).map_err(|e| format!("Download failed: {e}"))? != 512 {
        return Err("The update is damaged".into());
    }
    let image_len = u32::from_le_bytes([manifest[20], manifest[21], manifest[22], manifest[23]]);
    let sectors = image_len.div_ceil(512);
    if image_len == 0 || src.length() != 512 + 512 * u64::from(sectors) || sectors + 1 > slot {
        return Err("The update does not fit this phone".into());
    }
    // The supervisor checks the manifest before anything else is fetched.
    stage(&manifest).map_err(|e| format!("Couldn't stage: {e}"))?;
    if !sys("update manifest").starts_with("ok") {
        return Err(refusal(8).into());
    }
    let checked = sys("update check");
    if !checked.starts_with("ok") {
        return Err(refusal(field(&checked, "refused").unwrap_or(8)).into());
    }
    let mut piece = vec![0u8; PIECE];
    let mut sector = 0u32;
    while sector < sectors {
        progress(Phase::Downloading { done: 512 * u64::from(sector), total: 512 * u64::from(sectors) });
        let n = src.read(&mut piece).map_err(|e| format!("Download failed: {e}"))?;
        if n == 0 || n % 512 != 0 {
            return Err("The download was cut off".into());
        }
        stage(&piece[..n]).map_err(|e| format!("Couldn't stage: {e}"))?;
        if !sys(&format!("update write {sector}")).starts_with("ok") {
            return Err(refusal(8).into());
        }
        sector += (n / 512) as u32;
    }
    let mut hashed = 0;
    while hashed < sectors {
        progress(Phase::Verifying { done: hashed, total: sectors });
        let n = (sectors - hashed).min(HASH_STEP);
        let answer = sys(&format!("update hash {hashed} {n}"));
        if answer.strip_prefix("ok ").and_then(|v| v.trim().parse::<u32>().ok()) != Some(hashed + n) {
            return Err(refusal(8).into());
        }
        hashed += n;
    }
    let applied = sys("update apply");
    if !applied.starts_with("ok") {
        return Err(refusal(field(&applied, "refused").unwrap_or(9)).into());
    }
    Ok(field(&applied, "version").unwrap_or(0))
}

pub struct App {
    pub exit: bool,
    phase: Arc<Mutex<Phase>>,
    /// The running system's version, once the supervisor has said.
    system: Option<u32>,
    slot: String,
    pressed: bool,
}

impl App {
    pub fn new() -> App {
        App { exit: false, phase: Arc::new(Mutex::new(Phase::Idle)), system: None, slot: String::new(), pressed: false }
    }
    fn phase(&self) -> Phase {
        self.phase.lock().unwrap().clone()
    }
    fn busy(&self) -> bool {
        matches!(self.phase(), Phase::Checking | Phase::Downloading { .. } | Phase::Verifying { .. })
    }
    pub fn enter(&mut self) {
        self.exit = false;
        self.pressed = false;
        let info = violet::sys("update info");
        let fields = violet::fields(&info);
        self.slot = match (fields.get("slot").map(String::as_str), fields.get("running")) {
            (Some("0") | None, _) => "No second system slot on this phone".into(),
            (_, Some(r)) if fields.get("trial").map(String::as_str) == Some("1") => format!("Running from slot {}, still on trial", r.to_uppercase()),
            (_, Some(r)) => format!("Running from slot {}", r.to_uppercase()),
            _ => String::new(),
        };
        // A check with no update staged still says which version runs.
        self.system = field(&violet::sys("update check"), "system");
        println!("settings: updates, system={:?} {}", self.system, self.slot);
    }
    pub fn animating(&self) -> bool {
        self.busy()
    }
    fn start(&mut self) {
        let Some(url) = URL else { return };
        if self.busy() {
            return;
        }
        *self.phase.lock().unwrap() = Phase::Checking;
        let phase = self.phase.clone();
        std::thread::spawn(move || {
            println!("settings: update from {url}");
            let set = |p: Phase| *phase.lock().unwrap() = p;
            let result = match crate::fetch::Download::open(url) {
                Ok(mut d) => install(&mut d, &mut |line| violet::sys(line), &mut |bytes| std::fs::write(STAGING, bytes),
                                     &mut |p| set(p)),
                Err(e) => Err(format!("Couldn't download: {e}")),
            };
            let _ = std::fs::remove_file(STAGING);
            match result {
                Ok(v) => {
                    println!("settings: update staged version={v}");
                    set(Phase::Ready(v));
                }
                Err(e) => {
                    println!("settings: update failed: {e}");
                    set(Phase::Failed(e));
                }
            }
        });
    }
    pub fn touch(&mut self, kind: u16, x: i32, y: i32) {
        match kind {
            TOUCH_DOWN => self.pressed = INSTALL.contains(x, y),
            TOUCH_UP => {
                if HOME_BOX.contains(x, y) && !self.busy() {
                    self.exit = true;
                } else if self.pressed && INSTALL.contains(x, y) {
                    self.start();
                }
                self.pressed = false;
            }
            _ => {}
        }
    }
    pub fn draw(&self, c: &mut Canvas, f: &Fonts) {
        draw(c, f, &self.phase(), self.system, &self.slot, self.pressed);
    }
}

pub fn draw(c: &mut Canvas, f: &Fonts, phase: &Phase, system: Option<u32>, slot: &str, pressed: bool) {
    c.fill(c.bounds(), PAPER);
    violet::button(c, &f.medium(24.0), HOME_BOX, "‹ Settings", PAPER_RAISED, CREAM);
    f.regular(48.0).draw(c, "Updates", 40, 180, CREAM);
    let card = Rect::new(40, 230, W as i32 - 40, 420);
    slab(c, card, 30.0, PAPER_RAISED);
    f.medium(23.0).draw(c, "THIS SYSTEM", 80, 284, MUTED);
    let version = system.map_or("VioletOS".to_string(), |v| format!("VioletOS, version {v}"));
    f.medium(34.0).draw(c, &version, 80, 336, CREAM);
    f.regular(26.0).draw(c, slot, 80, 382, MUTED);
    let (line, tint) = match phase {
        Phase::Idle if URL.is_none() => ("This build has no update server".to_string(), MUTED),
        Phase::Idle => ("Updates come signed by VioletOS's own key".to_string(), MUTED),
        Phase::Checking => ("Checking the update…".to_string(), CREAM),
        Phase::Downloading { done, total } => (format!("Downloading… {}%", done * 100 / (*total).max(1)), CREAM),
        Phase::Verifying { done, total } => (format!("Verifying… {}%", u64::from(*done) * 100 / u64::from(*total).max(1)), CREAM),
        Phase::Ready(v) => (format!("Version {v} is ready. Restart the phone to start it."), GOOD),
        Phase::Failed(e) => (e.clone(), ALERT),
    };
    let face = f.regular(28.0);
    face.draw(c, &face.fit(&line, W as i32 - 88), 44, 500, tint);
    let fraction = match phase {
        Phase::Downloading { done, total } => Some(*done as f32 / (*total).max(1) as f32 * 0.8),
        Phase::Verifying { done, total } => Some(0.8 + *done as f32 / (*total).max(1) as f32 * 0.2),
        _ => None,
    };
    if let Some(p) = fraction {
        let bar = Rect::new(44, 560, W as i32 - 44, 584);
        c.round_rect(bar, 12.0, PAPER_RAISED);
        c.round_rect(Rect::new(bar.x0, bar.y0, bar.x0 + ((bar.width() as f32) * p.clamp(0.0, 1.0)) as i32, bar.y1), 12.0, VIOLET);
    }
    let note = "The new system starts as a trial. It is kept once it has run without trouble; if it doesn't start properly, the phone goes back to this one by itself.";
    let small = f.regular(25.0);
    let mut y = 640;
    let mut text = String::new();
    for word in note.split_whitespace() {
        let next = if text.is_empty() { word.to_string() } else { format!("{text} {word}") };
        if small.width(&next) > W as i32 - 88 && !text.is_empty() {
            small.draw(c, &text, 44, y, MUTED);
            y += 36;
            text = word.to_string();
        } else {
            text = next;
        }
    }
    small.draw(c, &text, 44, y, MUTED);
    if URL.is_some() && !matches!(phase, Phase::Ready(_)) {
        let busy = matches!(phase, Phase::Checking | Phase::Downloading { .. } | Phase::Verifying { .. });
        let bg = if pressed { PAPER_PRESSED } else if busy { PAPER_RAISED } else { VIOLET };
        violet::button(c, &f.medium(34.0), INSTALL, "Download and install", bg, if busy { MUTED } else { PAPER });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bytes(Vec<u8>, usize);
    impl Source for Bytes {
        fn length(&self) -> u64 {
            self.0.len() as u64
        }
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.0.len() - self.1);
            buf[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
            self.1 += n;
            Ok(n)
        }
    }
    /// A bundle of `sectors` image sectors (the manifest's signature is the
    /// supervisor's business; here only the layout matters).
    fn bundle(sectors: u32) -> Vec<u8> {
        let mut b = vec![0u8; 512 + 512 * sectors as usize];
        b[..4].copy_from_slice(b"VOT1");
        b[20..24].copy_from_slice(&(sectors * 512 - 100).to_le_bytes());
        for (i, x) in b[512..].iter_mut().enumerate() {
            *x = i as u8;
        }
        b
    }
    /// A model of the runtime's `update` commands, recording what was written.
    struct Phone {
        log: Vec<String>,
        staged: Vec<u8>,
        slot: Vec<u8>,
        manifest: Vec<u8>,
        check: &'static str,
        apply: &'static str,
        trial: bool,
    }
    impl Phone {
        fn new() -> Phone {
            Phone { log: vec![], staged: vec![], slot: vec![0; 4096 * 512], manifest: vec![], check: "ok version=3 system=2", apply: "ok version=3 system=2", trial: false }
        }
        fn sys(&mut self, line: &str) -> String {
            self.log.push(line.to_string());
            let w: Vec<&str> = line.split_whitespace().collect();
            match w[1] {
                "info" => format!("slot=4096 running=a trial={}", self.trial as u8),
                "manifest" => { self.manifest = std::mem::take(&mut self.staged); "ok 1".into() }
                "check" => self.check.into(),
                "write" => {
                    let at: usize = w[2].parse().unwrap();
                    let data = std::mem::take(&mut self.staged);
                    self.slot[at * 512..at * 512 + data.len()].copy_from_slice(&data);
                    format!("ok {}", data.len() / 512)
                }
                "hash" => format!("ok {}", w[2].parse::<u32>().unwrap() + w[3].parse::<u32>().unwrap()),
                "apply" => self.apply.into(),
                _ => "error".into(),
            }
        }
    }
    fn run(phone: &mut Phone, b: Vec<u8>) -> Result<u32, String> {
        let cell = std::cell::RefCell::new(phone);
        let mut phases = vec![];
        let r = install(&mut Bytes(b, 0), &mut |l| cell.borrow_mut().sys(l), &mut |d| { cell.borrow_mut().staged = d.to_vec(); Ok(()) }, &mut |p| phases.push(p));
        assert!(phases.iter().all(|p| matches!(p, Phase::Downloading { .. } | Phase::Verifying { .. })));
        r
    }

    #[test]
    fn a_bundle_is_checked_first_then_written_hashed_and_applied() {
        let mut phone = Phone::new();
        let b = bundle(300);
        assert_eq!(run(&mut phone, b.clone()), Ok(3));
        assert_eq!(phone.manifest, b[..512].to_vec());
        assert_eq!(&phone.slot[..300 * 512], &b[512..]);
        let verbs: Vec<&str> = phone.log.iter().map(|l| l.split_whitespace().nth(1).unwrap()).collect();
        assert_eq!(&verbs[..4], ["info", "manifest", "check", "write"]);
        assert_eq!(verbs.last(), Some(&"apply"));
        assert_eq!(phone.log.iter().filter(|l| l.contains("write")).count(), 3, "64 KiB pieces: 128 + 128 + 44 sectors");
        assert!(phone.log.contains(&"update hash 0 300".to_string()));
    }

    #[test]
    fn refusals_stop_before_the_image_is_sent() {
        let mut phone = Phone::new();
        phone.check = "error refused=5 system=4";
        assert_eq!(run(&mut phone, bundle(10)), Err(refusal(5).into()));
        assert!(!phone.log.iter().any(|l| l.contains("write")));
        let mut phone = Phone::new();
        phone.apply = "error refused=7 system=2";
        assert_eq!(run(&mut phone, bundle(10)), Err(refusal(7).into()));
    }

    #[test]
    fn damaged_or_oversized_bundles_are_not_sent() {
        let mut phone = Phone::new();
        let mut b = bundle(10);
        b.truncate(b.len() - 512);
        assert!(run(&mut phone, b).is_err());
        assert_eq!(phone.log, ["update info"]);
        let mut phone = Phone::new();
        assert!(run(&mut phone, bundle(4096)).is_err(), "the slot's last sector holds the manifest");
        assert_eq!(phone.log, ["update info"]);
    }

    #[test]
    fn nothing_is_sent_while_this_system_is_a_trial() {
        let mut phone = Phone::new();
        phone.trial = true;
        assert_eq!(run(&mut phone, bundle(10)), Err(refusal(10).into()));
        assert_eq!(phone.log, ["update info"]);
        // A runtime that does not say counts as a trial too.
        let mut phone = Phone::new();
        let r = install(&mut Bytes(bundle(10), 0), &mut |l| { phone.log.push(l.into()); "slot=4096 running=a".into() },
                        &mut |_| Ok(()), &mut |_| {});
        assert_eq!(r, Err(refusal(10).into()));
    }

    /// SETTINGS_PREVIEW=dir also writes the Updates page in three states.
    #[test]
    fn preview() {
        let Ok(dir) = std::env::var("SETTINGS_PREVIEW") else { return };
        let fonts = Fonts::load();
        let mut c = Canvas::new(violet::W, violet::H);
        for (name, phase) in [("updates", Phase::Idle), ("updates-progress", Phase::Downloading { done: 6 << 20, total: 20 << 20 }),
                              ("updates-ready", Phase::Ready(3))] {
            draw(&mut c, &fonts, &phase, Some(2), "Running from slot A", false);
            let bytes: Vec<u8> = c.px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255]).collect();
            std::fs::write(format!("{dir}/{name}.rgba"), bytes).unwrap();
        }
    }
}
