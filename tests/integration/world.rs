//! The scenario API: one `World` is one phone, built in memory.
//!
//! ```ignore
//! let mut w = World::boot(Image::new().app("notes", "").app("settings", "caps: system wifi-admin\n"));
//! let notes = w.open_app("notes");                       // as if tapped on Home
//! assert_eq!(w.app(notes).open("/data/settings", O_RDONLY), Err(ENOENT));
//! assert!(w.app(notes).sys("wifi join 0 00").starts_with("error not allowed"));
//! w.advance_secs(20);                                    // the fake clock
//! w.tap(360, 1300);                                      // the fake touch controller
//! assert!(w.log().contains("policy: notes"));           // the shipped code's own log lines
//! let w = w.reboot();                                    // /data survives, like the disk
//! ```
//!
//! Everything a World touches is on its own thread (kernel/), so tests
//! build as many as they like and run in parallel.
use crate::kernel::{self, clock, serial, Screen, Touch};
use crate::open::front::Touched;
use crate::open::policy::Cap;
use crate::open::vma::Access;
use crate::open::{self, entropy, fs, lifecycle, manifest, names, pool, shield, slots, trusted_ui, Entry, Runtime, Store, MAX_APPS};
pub use crate::open::syscall::*;

/// What is in the boot image: programs under /apps with their manifests,
/// and any other file.
#[derive(Clone, Default)]
pub struct Image {
    files: Vec<(String, Vec<u8>)>,
    manifests: String,
}

/// A small static ARM program like the shipped apps: constants, code, and
/// data + bss, each in its own 64 KiB pages. `tag` makes its bytes (and so
/// its hash) differ from another app's.
pub fn program(tag: &str) -> Vec<u8> {
    let mut rodata = vec![0x11u8; 0xd8c];
    rodata[..tag.len().min(64)].copy_from_slice(&tag.as_bytes()[..tag.len().min(64)]);
    elf(2, 0x20d90, &[(0x10000, rodata, 0xd8c, 4), (0x20d90, vec![0x22; 0x8f60], 0x8f60, 5), (0x39cf0, vec![0x33; 0x3c], 0x12310, 6)])
}
/// A 32-bit ARM ELF with the given PT_LOAD segments: (vaddr, file bytes, memsz, PF_* flags).
pub fn elf(kind: u16, entry: u32, segments: &[(u32, Vec<u8>, u32, u32)]) -> Vec<u8> {
    let phnum = segments.len();
    let mut image = vec![0u8; 52 + 32 * phnum];
    image[..4].copy_from_slice(b"\x7fELF");
    image[4] = 1;
    image[5] = 1;
    image[16..18].copy_from_slice(&kind.to_le_bytes());
    image[18..20].copy_from_slice(&40u16.to_le_bytes());
    image[24..28].copy_from_slice(&entry.to_le_bytes());
    image[28..32].copy_from_slice(&52u32.to_le_bytes());
    image[42..44].copy_from_slice(&32u16.to_le_bytes());
    image[44..46].copy_from_slice(&(phnum as u16).to_le_bytes());
    for (i, (vaddr, bytes, memsz, flags)) in segments.iter().enumerate() {
        let offset = image.len() as u32;
        image.extend_from_slice(bytes);
        let h = 52 + 32 * i;
        for (k, w) in [1, offset, *vaddr, *vaddr, bytes.len() as u32, *memsz, *flags, 0x10000].iter().enumerate() {
            image[h + 4 * k..h + 4 * k + 4].copy_from_slice(&w.to_le_bytes());
        }
    }
    image
}

impl Image {
    /// Home, Settings and the Agent with the manifests the phone ships
    /// (apps/open/*/manifest.txt, as tools/build_initfs.py packs them).
    pub fn new() -> Image {
        Image::default()
            .app("home", include_str!("../../apps/open/home/manifest.txt"))
            .app("settings", include_str!("../../apps/open/settings/manifest.txt"))
            .app("agent", include_str!("../../apps/open/agent/manifest.txt"))
    }
    /// An empty image (no Home): for tests of the loader and memory alone.
    pub fn bare() -> Image {
        Image::default()
    }
    /// /apps/NAME from the image with `manifest` ("" for none).
    pub fn app(mut self, name: &str, manifest: &str) -> Image {
        self.files.push((format!("apps/{name}"), program(name)));
        if !manifest.is_empty() {
            self.manifests += &format!("@{name}\n{manifest}");
            if !manifest.ends_with('\n') {
                self.manifests.push('\n');
            }
        }
        self
    }
    pub fn file(mut self, path: &str, data: &[u8]) -> Image {
        self.files.push((path.trim_start_matches('/').to_string(), data.to_vec()));
        self
    }
    /// The image as the phone gets it: a cpio "newc" archive.
    fn cpio(&self) -> &'static [u8] {
        let mut out = Vec::new();
        let mut files = self.files.clone();
        files.push(("apps/.manifests".to_string(), self.manifests.clone().into_bytes()));
        files.push(("TRAILER!!!".to_string(), Vec::new()));
        for (name, data) in &files {
            let mut fields = [0u32; 13];
            fields[1] = 0o100755;
            fields[6] = data.len() as u32;
            fields[11] = name.len() as u32 + 1;
            out.extend_from_slice(b"070701");
            for f in fields {
                out.extend_from_slice(format!("{f:08x}").as_bytes());
            }
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            while out.len() % 4 != 0 {
                out.push(0);
            }
            out.extend_from_slice(data);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
        // The file system borrows the image for the life of the boot, as on the phone.
        Box::leak(out.into_boxed_slice())
    }
}

/// What a restart keeps: the image and the files Storage holds for /data.
#[derive(Clone, Default)]
pub struct Persisted {
    /// Folders under /data, parents first (writeback.rs makes them too).
    pub dirs: Vec<String>,
    pub data: Vec<(String, Vec<u8>)>,
    pub installed: Vec<(Entry, Vec<u8>)>,
    /// Storage could not list every file at boot (data.rs `load` -> false).
    pub unreadable: bool,
}

pub struct World {
    pub rt: Runtime,
    pub screen: Screen,
    pub touch: Touch,
    image: Image,
    installed: Vec<(Entry, Vec<u8>)>,
}

/// Seeds differ per World so two phones never share a layout by accident.
fn seed(n: u64) -> [u8; 48] {
    let mut s = [0u8; 48];
    for (i, b) in s.iter_mut().enumerate() {
        *b = (n.wrapping_mul(0x9e3779b97f4a7c15).rotate_left(i as u32 * 7) >> 11) as u8 ^ i as u8;
    }
    s
}

impl World {
    /// Boot a phone with this image and an empty /data.
    pub fn boot(image: Image) -> World {
        World::boot_with(image, Persisted::default(), 1)
    }
    /// Boot with what an earlier boot left (`World::shutdown`) and a seed
    /// standing in for this boot's timer jitter.
    pub fn boot_with(image: Image, kept: Persisted, boot_seed: u64) -> World {
        World::boot_keyed(image, kept, boot_seed, true)
    }
    /// As `boot_with`, but the Sentinel has no sealing key (QEMU with no
    /// valid root key blob, system/sentinel/process.rs `keyed_engine`).
    pub fn boot_unkeyed(image: Image, kept: Persisted, boot_seed: u64) -> World {
        World::boot_keyed(image, kept, boot_seed, false)
    }
    fn boot_keyed(image: Image, kept: Persisted, boot_seed: u64, keyed: bool) -> World {
        kernel::reset();
        // procmgr.rs's frame ledger is per thread on the host and outlives a
        // World: a new boot starts with no page claimed by any address space,
        // as the phone's does (the old boot's spaces are gone with it).
        *open::procmgr::ledger() = open::procmgr::frames::Frames::new(0, u32::MAX);
        trusted_ui::reset();
        // The Sentinel is a process of the phone: a boot starts it empty (it
        // is per test thread here), so only what /data kept sealed comes back.
        if keyed {
            open::policy::port::restart();
        } else {
            open::policy::port::restart_unkeyed();
        }
        let mut fs = fs::Fs::new(image.cpio());
        for dir in &kept.dirs {
            let _ = fs.mkdir(dir);
        }
        for (path, data) in &kept.data {
            if let Some(f) = fs.writable(path, true, true, 0) {
                f.extend_from_slice(data);
            }
        }
        let mut store = Store::default();
        for (entry, program) in &kept.installed {
            if let Some(f) = fs.writable(&format!("/apps/{}", entry.name), true, true, 0) {
                f.extend_from_slice(program);
            }
            store.entries.push(entry.clone());
        }
        // mod.rs `Runtime::new`: the passcode first, then the policy's book.
        trusted_ui::boot(&fs, !kept.unreadable);
        let book = manifest::Book::load(&mut fs);
        let mut drbg = entropy::Drbg::new(open::sha256);
        drbg.seed(&seed(boot_seed));
        let pool = pool::Pool::new();
        let rt = Runtime {
            k: pool::Kernel::new(),
            reserve: pool.total / 8,
            pool,
            fs,
            store,
            apps: core::array::from_fn(|_| None),
            book,
            focused: true,
            foreground: None,
            home: None,
            next_pid: 100,
            entropy: drbg,
            shield: shield::ShieldPolicy::new(),
            last_touch_ns: 0,
            life: lifecycle::Book::default(),
            slots: slots::Table::new(),
            names: names::Names::new(),
            pipes: open::loopback::Budget::new(),
        };
        let mut w = World { rt, screen: Screen::default(), touch: Touch::default(), image, installed: kept.installed };
        if w.rt.fs.stat(open::HOME_APP).is_some() {
            let home = w.rt.launch(open::HOME_APP, None).expect("Home starts");
            w.rt.home = Some(home);
            w.rt.set_foreground(Some(home));
        }
        w.settle();
        w
    }
    /// Power off: what the next boot finds: every folder and file under
    /// /data that the shipped write-back sends to Storage
    /// (services/storage/writeback.rs `key`, which leaves out a full-system
    /// update's `*.ota-staging` pieces), nested folders included.
    ///
    /// This is a clean shutdown: the write-back has sent everything. What a
    /// sudden power cut keeps (only what was committed, data.rs `fsync`) is
    /// Storage's own host tests (services/storage/tests/writeback_tests.rs).
    pub fn shutdown(self) -> (Image, Persisted) {
        let (mut dirs, mut data) = (Vec::new(), Vec::new());
        let mut todo = vec![String::from("/data")];
        while let Some(dir) = todo.pop() {
            for (name, is_dir) in self.rt.fs.list(&dir) {
                let path = format!("{dir}/{name}");
                if crate::storage::writeback::key(&path).is_none() {
                    continue;
                }
                if is_dir {
                    dirs.push(path.clone());
                    todo.push(path);
                } else if let Some(bytes) = self.rt.fs.contents(&path) {
                    data.push((path, bytes.to_vec()));
                }
            }
        }
        dirs.sort_by_key(|d| d.matches('/').count());
        (self.image, Persisted { dirs, data, installed: self.installed, unreadable: false })
    }
    /// Restart: a new boot on the same image and disk.
    pub fn reboot(self) -> World {
        let (image, kept) = self.shutdown();
        World::boot_with(image, kept, 2)
    }

    // ---- apps ----

    /// Start /apps/NAME and put it in front, as a tap on its Home icon does.
    pub fn open_app(&mut self, name: &str) -> usize {
        let path = format!("/apps/{name}");
        let i = self.rt.launch(&path, None).unwrap_or_else(|e| panic!("{path}: {e}"));
        self.rt.set_foreground(Some(i));
        self.settle();
        i
    }
    /// Start a program without bringing it to the front.
    pub fn start(&mut self, path: &str, caller: Option<usize>) -> Result<usize, String> {
        self.rt.launch(path, caller)
    }
    /// A child process of app `parent` (fork): same program, same owner.
    pub fn fork(&mut self, parent: usize) -> usize {
        let (path, ppid) = self.rt.apps[parent].as_ref().map(|a| (a.path.clone(), a.pid)).unwrap();
        let i = self.rt.launch(&path, Some(parent)).expect("fork");
        self.rt.apps[i].as_mut().unwrap().sig.ppid = ppid;
        i
    }
    /// What Violet Agent does: /apps/NAME written since boot, with the
    /// manifest it was installed with (appstore.rs keeps it).
    pub fn install(&mut self, name: &str, manifest: &str) {
        let bytes = program(&format!("installed-{name}"));
        self.rt.forget_consent(name);
        let f = self.rt.fs.writable(&format!("/apps/{name}"), true, true, 0).expect("/apps is there");
        f.extend_from_slice(&bytes);
        let entry = Entry { name: name.to_string(), package: false, manifest: manifest.to_string() };
        self.rt.store.entries.retain(|e| e.name != name);
        self.rt.store.entries.push(entry.clone());
        self.installed.retain(|e| e.0.name != name);
        self.installed.push((entry, bytes));
    }
    pub fn uninstall(&mut self, name: &str) {
        self.rt.store.entries.retain(|e| e.name != name);
        self.installed.retain(|e| e.0.name != name);
        let _ = self.rt.fs.unlink(&format!("/apps/{name}"), false);
        self.rt.forget_app(name);
    }
    /// Bring app `i` to the front (None: Home).
    pub fn front(&mut self, i: Option<usize>) {
        let next = i.or(self.rt.home);
        self.rt.set_foreground(next);
        self.settle();
    }
    pub fn app(&mut self, i: usize) -> AppHandle<'_> {
        AppHandle { w: self, i }
    }
    pub fn running(&self, i: usize) -> bool {
        self.rt.apps[i].is_some()
    }
    pub fn state(&self, i: usize) -> lifecycle::State {
        self.rt.apps[i].as_ref().expect("the app runs").life.state
    }

    // ---- time, touch, screen ----

    /// One pass of the runtime's loop.
    pub fn settle(&mut self) {
        self.rt.ui_tick();
        self.rt.lifecycle_tick();
        self.rt.ui_draw(&mut self.screen);
    }
    /// Time passes, a tenth of a second at a time, the loop running throughout.
    pub fn advance_ms(&mut self, ms: u64) {
        let mut left = ms;
        while left > 0 {
            let step = left.min(100);
            clock::advance_ns(step * 1_000_000);
            left -= step;
            self.settle();
        }
    }
    pub fn advance_secs(&mut self, secs: u64) {
        self.advance_ms(secs * 1000);
    }
    /// A finger goes down at (x, y) and lifts 150 ms later. Returns the app
    /// that got the touch, if any did: system UI takes its own, and nothing
    /// on the band's rows reaches an app (device.rs).
    pub fn tap(&mut self, x: i32, y: i32) -> Option<usize> {
        let got = self.finger(x, y, true);
        self.advance_ms(150);
        self.finger(x, y, false);
        self.advance_ms(50);
        got
    }
    pub fn finger(&mut self, x: i32, y: i32, down: bool) -> Option<usize> {
        if down { self.touch.press(x, y) } else { self.touch.release(x, y) }
        let mut got = None;
        while let Some(r) = self.touch.reports.pop_front() {
            trusted_ui::with(|s| s.down = r.down);
            if self.rt.ui_touch(r.x, r.y, r.down) {
                continue;
            }
            if (r.y as u32) < open::sysui::BAND_H {
                continue;
            }
            if r.down {
                self.rt.last_touch_ns = clock::now_ns();
            }
            got = self.rt.foreground.filter(|_| self.rt.focused);
        }
        self.settle();
        got
    }
    /// The app in front presents `color` over its whole surface. The
    /// runtime copies only the rows that are the app's (sysui.rs `app_rows`).
    pub fn present(&mut self, i: usize, color: u32) {
        if self.rt.foreground != Some(i) || !self.rt.focused {
            return;
        }
        if let Some((y0, y1)) = open::sysui::app_rows(0, open::sysui::HEIGHT, trusted_ui::covers()) {
            self.screen.fill(0, y0 as usize, kernel::WIDTH, (y1 - y0) as usize, color);
        }
        self.rt.ui_draw(&mut self.screen);
    }
    /// Type on the runtime's keypad: digits, then OK.
    pub fn enter(&mut self, pin: &str) {
        for d in pin.bytes() {
            let n = if d == b'0' { 10 } else { (d - b'1') as usize };
            self.key(n);
        }
        self.key(11);
        self.settle();
    }
    /// Tap key `n` of the pad (sysui.rs `pad_rect`: 0-8 are 1-9, 9 DEL, 10 is 0, 11 OK, 12 CANCEL).
    pub fn key(&mut self, n: usize) {
        let [x, y, w, h] = open::sysui::pad_rect(n);
        // Past the anti-tapjacking quiet time (sysui.rs `Taps`).
        self.advance_ms(800);
        self.tap((x + w / 2) as i32, (y + h / 2) as i32);
    }
    /// Answer the consent prompt on screen.
    pub fn answer_prompt(&mut self, allow: bool) {
        let [x, y, w, h] = open::sysui::prompt_rect(if allow { open::sysui::ALLOW } else { open::sysui::DENY });
        self.advance_ms(1200);
        self.tap((x + w / 2) as i32, (y + h / 2) as i32);
    }
    pub fn locked(&self) -> bool {
        trusted_ui::locked()
    }
    pub fn mode(&self) -> open::sysui::Mode {
        trusted_ui::mode()
    }

    // ---- what the shipped code logged ----

    pub fn log(&self) -> String {
        serial::text()
    }
    pub fn logged(&self, what: &str) -> usize {
        serial::lines().iter().filter(|l| l.contains(what)).count()
    }
    /// Mark the log; `since` gives the lines after a mark.
    pub fn mark(&self) -> usize {
        serial::lines().len()
    }
    pub fn since(&self, mark: usize) -> String {
        serial::lines()[mark..].join("\n")
    }
}

/// One running app, for scripted system calls.
pub struct AppHandle<'a> {
    w: &'a mut World,
    i: usize,
}

fn result(r: i32) -> Result<i32, i32> {
    if r < 0 && r > -4096 { Err(r) } else { Ok(r) }
}

impl AppHandle<'_> {
    pub fn pid(&self) -> u32 {
        self.w.rt.apps[self.i].as_ref().unwrap().pid
    }
    /// A raw Linux ARM system call: number and r0..r5.
    pub fn syscall(&mut self, n: u32, a: [u32; 6]) -> i32 {
        let r = self.w.rt.syscall(self.i, n, a);
        self.w.settle();
        r
    }
    /// Put a NUL-terminated string in the app's memory; its address.
    pub fn string(&mut self, at: u32, s: &str) -> u32 {
        let rt = &mut self.w.rt;
        let app = rt.apps[self.i].as_mut().unwrap();
        let va = app.scratch + at;
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        assert!(app.space.write(&mut rt.k, &mut rt.pool, va, &bytes), "the app's stack takes the string");
        va
    }
    pub fn open(&mut self, path: &str, flags: u32) -> Result<i32, i32> {
        let p = self.string(0, path);
        result(self.syscall(5, [p, flags, 0o644, 0, 0, 0]))
    }
    pub fn close(&mut self, fd: i32) -> Result<i32, i32> {
        result(self.syscall(6, [fd as u32, 0, 0, 0, 0, 0]))
    }
    pub fn stat(&mut self, path: &str) -> Result<i32, i32> {
        let p = self.string(0, path);
        result(self.syscall(195, [p, 0, 0, 0, 0, 0]))
    }
    pub fn unlink(&mut self, path: &str) -> Result<i32, i32> {
        let p = self.string(0, path);
        result(self.syscall(10, [p, 0, 0, 0, 0, 0]))
    }
    pub fn mkdir(&mut self, path: &str) -> Result<i32, i32> {
        let p = self.string(0, path);
        result(self.syscall(39, [p, 0o755, 0, 0, 0, 0]))
    }
    pub fn rename(&mut self, from: &str, to: &str) -> Result<i32, i32> {
        let (f, t) = (self.string(0, from), self.string(0x800, to));
        result(self.syscall(38, [f, t, 0, 0, 0, 0]))
    }
    /// open(O_WRONLY|O_CREAT|O_TRUNC), write, close: what `open(path, "w")` does.
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), i32> {
        let fd = self.open(path, O_WRONLY | O_CREAT | O_TRUNC)?;
        let r = self.write(fd, data);
        self.close(fd)?;
        r.map(|_| ())
    }
    pub fn read_file(&mut self, path: &str) -> Result<Vec<u8>, i32> {
        let fd = self.open(path, O_RDONLY)?;
        let r = self.read(fd, 4096);
        self.close(fd)?;
        r
    }
    pub fn write(&mut self, fd: i32, data: &[u8]) -> Result<i32, i32> {
        let rt = &mut self.w.rt;
        let app = rt.apps[self.i].as_mut().unwrap();
        let va = app.scratch + 0x1000;
        assert!(app.space.write(&mut rt.k, &mut rt.pool, va, data));
        result(self.syscall(4, [fd as u32, va, data.len() as u32, 0, 0, 0]))
    }
    /// ftruncate(fd, len) (93) or ftruncate64 (194, the length in r2:r3 as EABI passes it).
    pub fn ftruncate(&mut self, fd: i32, len: u32, wide: bool) -> Result<i32, i32> {
        result(if wide { self.syscall(194, [fd as u32, 0, len, 0, 0, 0]) } else { self.syscall(93, [fd as u32, len, 0, 0, 0, 0]) })
    }
    /// truncate(path, len).
    pub fn truncate(&mut self, path: &str, len: u32) -> Result<i32, i32> {
        let p = self.string(0, path);
        result(self.syscall(92, [p, len, 0, 0, 0, 0]))
    }
    pub fn read(&mut self, fd: i32, max: u32) -> Result<Vec<u8>, i32> {
        let va = self.w.rt.apps[self.i].as_ref().unwrap().scratch + 0x1000;
        let n = result(self.syscall(3, [fd as u32, va, max.min(0x1000), 0, 0, 0]))? as usize;
        let rt = &mut self.w.rt;
        let mut out = vec![0u8; n];
        let app = rt.apps[self.i].as_mut().unwrap();
        assert!(app.space.read(&mut rt.k, &mut rt.pool, va, &mut out));
        Ok(out)
    }
    /// The names open() + getdents show for a directory.
    pub fn listdir(&mut self, path: &str) -> Result<Vec<String>, i32> {
        let fd = self.open(path, O_RDONLY)?;
        let names = match self.w.rt.apps[self.i].as_ref().unwrap().fds[fd as usize].as_ref() {
            Some(open::File::Dir { entries, .. }) => entries.iter().map(|e| e.0.clone()).collect(),
            _ => return Err(ENOTDIR),
        };
        self.close(fd)?;
        Ok(names)
    }
    /// A command written to /dev/violet/sys, and the answer read back.
    pub fn sys(&mut self, line: &str) -> String {
        let fd = self.open("/dev/violet/sys", O_RDWR).expect("/dev/violet/sys opens for every app");
        self.write(fd, format!("{line}\n").as_bytes()).expect("the command is written");
        if !self.w.running(self.i) {
            return String::new();
        }
        let answer = self.read(fd, 4096).unwrap_or_default();
        let _ = self.close(fd);
        String::from_utf8_lossy(&answer).trim().to_string()
    }
    /// socket(domain, type, 0).
    pub fn socket(&mut self, domain: u32, kind: u32) -> Result<i32, i32> {
        result(self.syscall(281, [domain, kind, 0, 0, 0, 0]))
    }
    /// mmap2(addr, len, prot, flags, fd, 0): the address, or the error.
    pub fn mmap(&mut self, addr: u32, len: u32, prot: u32, flags: u32, fd: i32) -> Result<u32, i32> {
        result(self.syscall(192, [addr, len, prot, flags, fd as u32, 0])).map(|a| a as u32)
    }
    pub fn mprotect(&mut self, addr: u32, len: u32, prot: u32) -> Result<i32, i32> {
        result(self.syscall(125, [addr, len, prot, 0, 0, 0]))
    }
    pub fn munmap(&mut self, addr: u32, len: u32) -> Result<i32, i32> {
        result(self.syscall(91, [addr, len, 0, 0, 0, 0]))
    }
    pub fn brk(&mut self, to: u32) -> u32 {
        self.syscall(45, [to, 0, 0, 0, 0, 0]) as u32
    }
    /// rt_sigaction(sig, {handler, flags, 0, mask}, NULL, 8).
    pub fn sigaction(&mut self, sig: u32, handler: u32, flags: u32) -> Result<i32, i32> {
        let rt = &mut self.w.rt;
        let app = rt.apps[self.i].as_mut().unwrap();
        let va = app.scratch + 0x1800;
        let mut b = [0u8; 20];
        b[..4].copy_from_slice(&handler.to_le_bytes());
        b[4..8].copy_from_slice(&flags.to_le_bytes());
        assert!(app.space.write(&mut rt.k, &mut rt.pool, va, &b));
        result(self.syscall(174, [sig, va, 0, 8, 0, 0]))
    }
    pub fn kill(&mut self, pid: i32, sig: u32) -> Result<i32, i32> {
        result(self.syscall(37, [pid as u32, sig, 0, 0, 0, 0]))
    }
    /// Signals delivered to this app's handlers so far: (signal, handler).
    pub fn delivered(&self) -> Vec<(u32, u32)> {
        self.w.rt.apps[self.i].as_ref().unwrap().delivered.clone()
    }
    /// The app reads, writes or runs memory at `va`.
    pub fn touch(&mut self, va: u32, access: Access) -> Touched {
        let r = self.w.rt.touch(self.i, va, access);
        self.w.settle();
        r
    }
    /// Where the loader put things (procmgr.rs, vma.rs `Layout`).
    pub fn layout(&self) -> open::vma::Layout {
        self.w.rt.apps[self.i].as_ref().unwrap().space.layout
    }
    pub fn at_random(&self) -> [u8; 16] {
        self.w.rt.apps[self.i].as_ref().unwrap().random
    }
    pub fn entry(&self) -> u32 {
        self.w.rt.apps[self.i].as_ref().unwrap().entry
    }
    pub fn sp(&self) -> u32 {
        self.w.rt.apps[self.i].as_ref().unwrap().sp
    }
    pub fn holds(&self, cap: Cap) -> bool {
        self.w.rt.holds(self.i, cap)
    }
    /// The hardware mapping of the page at `va`: (writable, executable).
    pub fn mapping(&self, va: u32) -> Option<(bool, bool)> {
        kernel::sys::at(self.w.rt.apps[self.i].as_ref().unwrap().space.pd, va)
    }
}

/// The apps table is this big on every board.
pub const APPS: usize = MAX_APPS;

/// Put a service behind Runtime, the way an open app's request reaches one
/// on the phone (runtime/process.rs): the request is answered with a
/// ticket, and a POLL of the ticket with the service's result. A result
/// whose op is BUSY is never seen: Runtime answers BUSY while a job runs, so
/// the next poll finds the ticket gone (INVALID). `service` is the
/// service's own request handler.
pub fn serve_through_runtime(mut service: impl FnMut(crate::protocol::Message) -> crate::protocol::Message + 'static) {
    use crate::protocol::*;
    let mut tickets: Vec<(u32, Message)> = Vec::new();
    let mut next = 1u32;
    kernel::ipc::serve(kernel::sys::SLOT_RUNTIME, move |m: Message| {
        if m.op == POLL {
            return match tickets.iter().position(|t| t.0 == m.value) {
                Some(k) => {
                    let r = tickets.remove(k).1;
                    if r.op == BUSY { Message::error(INVALID) } else { r }
                }
                None => Message::error(INVALID),
            };
        }
        next += 1;
        tickets.push((next, service(m)));
        Message::result(next)
    });
}
