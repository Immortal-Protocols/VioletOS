//! Open Runtime ("Runtime v2"): runs unmodified Linux ARM EABI programs on
//! seL4. Each app is an seL4 address space whose threads have this process as
//! their fault handler. A Linux `svc #0` is an unknown syscall to seL4, so it
//! arrives here as a fault message; the reply carries the result. Page faults
//! map memory on demand. Built-in apps keep using Runtime, which knows this
//! process as client 9 (OPEN_APPS).
pub mod abi;
pub mod contain;
pub mod appstore;
pub mod data;
#[cfg(moto_board)]
pub mod display;
pub mod device;
pub mod frames;
pub mod fs;
pub mod hci;
pub mod heap;
pub mod inflate;
pub mod iov;
pub mod names;
pub mod net;
pub mod loopback;
pub mod sockaddr;
pub mod slots;
pub mod scanout;
pub mod sockets;
pub mod touch;
pub mod sysui;
pub mod trusted_ui;
pub mod policy;
pub mod manifest;
pub mod shield;
// scorer.rs and scorer_model.rs are built into the Sentinel (system/sentinel/), not here.
pub mod guard;
#[path = "../../services/sentinel/mod.rs"]
pub mod sentinel;
pub mod pool;
pub mod cores;
pub mod lifecycle;
pub mod vma;
pub mod signal;
pub mod entropy;
#[path = "../../system/supervisor/procmgr.rs"]
pub mod procmgr;
pub mod syscall;
pub mod sys;
pub mod wifi;
pub mod powerctl;
pub mod sntp;
pub mod timesync;
pub mod update;

use crate::protocol::*;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use policy::{Action, AppId};
use shield::{EventKind, ShieldPolicy, TelemetryEvent};
use pool::{Kernel, Pool};
use procmgr::Space;
use sys::Object;

/// The app table: the most apps that may run at once on any board. How many
/// actually may is `Runtime::app_limit`, set at boot from the kernel memory
/// slices that fit and what the pool holds (lifecycle::app_limit). Socket
/// floors (slots.rs) are kept for this many. Fault badges are app << 12.
pub const MAX_APPS: usize = 12;
/// Pages one more app behind the front is budgeted in the pool: its two
/// screen buffers and 2 MiB, for `app_limit`.
const PER_APP_PAGES: usize = 2 * device::BUFFER_BYTES / sys::PAGE + 2 * sys::MIB_PAGES;
/// The Home app: started at boot and shown whenever no other app is in front.
pub const HOME_APP: &str = "/apps/home";
const APP_KERNEL_BITS: u32 = 19; // 512 KiB of kernel objects per app
const APP_CNODE_BITS: u32 = 8;
/// Pages of the pool kept for the runtime's own heap.
const HEAP_PAGES: usize = sys::HEAP_PAGES; // 48 MiB: the runtime, pushed apps and file data (pages.rs)

pub struct Log;
impl Log {
    fn page() -> *mut u32 {
        sys::LOG_PAGE as *mut u32
    }
    pub fn init() {
        unsafe {
            Self::page().add(1).write_volatile(0);
            Self::page().write_volatile(0x474f4c4f); // "OLOG"
        }
    }
    /// One line of the log (the `log!` macro), with its prefix.
    #[inline(never)]
    pub fn line(args: core::fmt::Arguments) {
        Self::write(alloc::format!("[open] {}\n", args).as_bytes());
    }
    /// Append to the ring: word 0 magic, word 1 bytes written in total, data from byte 8.
    pub fn write(bytes: &[u8]) {
        const SIZE: u32 = 4096 - 8;
        unsafe {
            let head = Self::page().add(1).read_volatile();
            let data = (sys::LOG_PAGE + 8) as *mut u8;
            for (i, b) in bytes.iter().enumerate() {
                data.add(((head + i as u32) % SIZE) as usize).write_volatile(*b);
            }
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            Self::page().add(1).write_volatile(head.wrapping_add(bytes.len() as u32));
        }
    }
}
#[macro_export]
macro_rules! log {
    // One call per line logged; the text is put together in `Log::line`.
    ($($arg:tt)*) => {
        $crate::process::Log::line(format_args!($($arg)*))
    };
}

#[derive(Clone)]
pub enum Wait {
    Futex { app: usize, addr: u32 },
    Sleep,
    Poll { fds: Vec<(i32, i16)>, ptr: u32, time64: bool },
    Epoll { epfd: i32, events: u32, max: u32 },
    Read { fd: i32, buf: u32, len: u32 },
    /// recvfrom/recvmsg: a read whose source address goes to `name`.
    RecvFrom { fd: i32, buf: u32, len: u32, name: u32, name_len: u32 },
    /// A blocking accept/accept4 on a listening socket (sockets.rs).
    Accept { fd: i32, addr: u32, len: u32, flags: u32 },
    /// A blocking write to /dev/violet/audio waiting for room in the ring.
    Audio { fd: i32, buf: u32, len: u32 },
    /// A blocking connect, or a send waiting for room in the socket's ring.
    Connect { fd: i32 },
    Send { fd: i32, buf: u32, len: u32 },
    /// Held by a Violet Shield freeze; answered with EACCES when the app thaws.
    Frozen,
    /// A call that finished while its app was frozen (lifecycle or Shield):
    /// answered with `result` once the app runs again (`wake`).
    Held { result: i32 },
    /// A present waiting for the panel to scan past its rows (scanout.rs);
    /// answers the write with `result`.
    Present { result: i32 },
    /// SCREEN_IOC_WAIT_VSYNC: until a frame after `after` starts; VsyncInfo to `ptr`.
    Vsync { after: u64, ptr: u32 },
    /// An open or socket that needs the user's answer to the consent prompt
    /// up for it (trusted_ui.rs `pending`): run again once it is answered
    /// or withdrawn, when policy has the answer.
    Consent { cap: policy::Cap },
    /// For a signal or a child (Thread.sig.waiting says which: pause,
    /// sigsuspend, sigtimedwait, wait4, a vfork parent); only syscall.rs
    /// wakes it. A timeout (sigtimedwait) answers EAGAIN.
    Signal,
}

/// The outcome of the Shield check at the choke point.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Allow,
    Deny,
    /// The app was frozen; the calling thread waits until it thaws.
    Frozen,
    /// The app was ended.
    Killed,
}

pub struct Frozen {
    pub until_ns: u64,
    pub shield_id: u32,
    /// Threads this freeze suspended (the rest were already blocked in a call).
    pub suspended: Vec<usize>,
}

/// Bytes an app has sent on a connection, reported as SOCK_TX.
pub struct TxStat {
    pub pid: u32,
    pub index: usize,
    pub host: String,
    pub port: u16,
    pub pending: u32,
    pub reported: u64,
    pub last_ns: u64,
}

pub struct Thread {
    pub tid: u32,
    pub tcb: u32,
    pub reply: Option<u32>,
    pub wait: Option<Wait>,
    pub deadline: Option<u64>,
    /// The fault message to answer with when it wakes: r0..r7 and FaultIP.
    pub saved: [u32; 9],
    pub clear_tid: u32,
    pub alive: bool,
    pub slot: u32,
    /// Its scheduling context (MCS), or 0.
    pub sc: u32,
    /// Its signal mask, pending signals and alternate stack (signal.rs).
    pub sig: signal::Thread<syscall::Vfork>,
}

pub type FileRef = Rc<RefCell<device::OpenFile>>;

pub struct App {
    pub pid: u32,
    pub name: String,
    pub path: String,
    pub space: Space,
    pub cnode: u32,
    pub threads: Vec<Thread>,
    pub fds: Vec<Option<FileRef>>,
    pub cwd: String,
    pub events: alloc::collections::VecDeque<abi::InputEvent>,
    pub surface: Option<device::Surface>,
    pub exit_code: Option<i32>,
    /// SHA-256 of the program file (the script, for interpreted programs).
    pub digest: [u8; 32],
    /// Not part of the boot image (written or pushed since boot).
    pub new_hash: bool,
    /// Violet Shield: the app this program belongs to (shield.rs `owner_of`).
    pub owner: String,
    pub frozen: Option<Frozen>,
    /// When it was last in front (ns), to close the least recently used first.
    pub used_ns: u64,
    /// Foreground, visible, cached or frozen (lifecycle.rs).
    pub life: lifecycle::Life,
    /// Signal actions, the parent and children, timers (signal.rs).
    pub sig: signal::Proc<syscall::CloseOnExec>,
}

/// Room the pool keeps for the app in front: a launch closes background apps
/// until this much is free (32 MiB).
const RESERVE_PAGES: usize = 32 * sys::MIB_PAGES;

pub struct Runtime {
    pub k: Kernel,
    pub pool: Pool,
    pub fs: fs::Fs,
    /// Installed apps kept on disk (appstore.rs).
    pub store: appstore::Store,
    pub apps: [Option<App>; MAX_APPS],
    untypeds: Vec<u32>,
    /// The capability policy: the image's manifests and the user's answers (manifest.rs).
    pub book: manifest::Book,
    /// Runtime has put this process in front.
    pub focused: bool,
    /// The app on screen while focused; None shows the app list.
    pub foreground: Option<usize>,
    /// The running Home app, if /apps/home exists.
    pub home: Option<usize>,
    home_tried: bool,
    pub shell: device::Shell,
    next_pid: u32,
    /// The random generator behind getrandom, AT_RANDOM and app layouts.
    pub entropy: entropy::Drbg,
    contact: Option<u32>,
    last_poll: u64,
    /// Loop statistics, logged every 30 s while apps run.
    pub stats: Stats,
    usb_serial: u32,
    /// Audio is installed, so its PCM ring is mapped at 0x09100000.
    pub audio: bool,
    /// The Camera service's page and frames are mapped.
    pub camera: bool,
    pub net: net::Net,
    pub shield: ShieldPolicy,
    pub scorer: guard::ScoreLink,
    pub sentinel: sentinel::Sentinel,
    pub last_touch_ns: u64,
    pub tx_stats: Vec<Option<TxStat>>,
    last_sentinel: u64,
    /// The "wifi" commands' running Radio requests.
    pub wifi: wifi::Wifi,
    /// Present cost, input-to-frame latency and touch polling (frames.rs).
    pub frames: frames::Frames,
    /// The panel's scan line and frame count (scanout.rs).
    pub beam: scanout::Beam,
    /// Reports Touch reads on its interrupt (touch.rs, handset).
    pub ring: touch::Ring,
    /// The app whose threads run in the foreground tier (MCS), as last applied.
    tiered: Option<usize>,
    /// How many apps may run at once (at most MAX_APPS; see lifecycle::app_limit).
    pub app_limit: usize,
    /// Memory pressure and the apps closed for memory (lifecycle.rs).
    pub life: lifecycle::Book,
}

/// A request to Runtime, waiting for its ticket like the app SDK does.
pub fn request(op: u32, key: u32, value: u32) -> Message {
    request_with(op, key, value, 0)
}
/// As `request`, with the argument word (a tone's pitch and length).
pub fn request_with(op: u32, key: u32, value: u32, arg: u32) -> Message {
    let r = Message::new(op, key, value).with_arg(arg).call(sys::SLOT_RUNTIME);
    if r.op != OK {
        return Message::error(r.op);
    }
    loop {
        let p = Message::new(POLL, 0, r.value).call(sys::SLOT_RUNTIME);
        if p.op != BUSY {
            return p;
        }
        sys::pause();
    }
}

fn badge(app: usize, thread: usize) -> u32 {
    ((app as u32) << 12) | (thread as u32 + 1)
}

impl Runtime {
    fn new() -> Result<Runtime, i32> {
        let mut k = Kernel::new();
        // Each step fails cleanly if the supervisor could not provide its part.
        sys::retype(sys::SLOT_KERNEL, Object::Endpoint, 0, sys::SLOT_FAULT_EP).map_err(|e| { log!("no kernel memory"); e })?;
        let (mut pool, _) = Pool::new(&mut k, HEAP_PAGES).map_err(|e| { log!("no app memory pool"); e })?;
        // 64 KiB for Reply objects first, then as many 512 KiB app slices as
        // kernel memory holds, up to the table: the supervisor's 8 MiB (4 MiB
        // on an older one) less the pool's page tables.
        k.replies = k.object(sys::SLOT_KERNEL, Object::Untyped, 16).unwrap_or(0);
        let mut untypeds = Vec::new();
        while untypeds.len() < MAX_APPS {
            match k.object(sys::SLOT_KERNEL, Object::Untyped, APP_KERNEL_BITS) {
                Ok(u) => untypeds.push(u),
                Err(e) if untypeds.is_empty() => return Err(e),
                Err(_) => break,
            }
        }
        let mut image: &'static [u8] =
            unsafe { core::slice::from_raw_parts(sys::INITFS as *const u8, sys::INITFS_BYTES) };
        // The handset's image is compressed: "VGZ1", the gzip length, the gzip.
        if &image[..4] == b"VGZ1" {
            let packed_len = u32::from_le_bytes(image[4..8].try_into().unwrap()) as usize;
            let packed = &image[8..(8 + packed_len).min(image.len())];
            let size = inflate::gzip_size(packed).unwrap_or(0);
            let base = pool.take_top((size + sys::PAGE - 1) / sys::PAGE);
            let Some(base) = base else {
                log!("no room to unpack the {} byte file system image", size);
                return Err(-1);
            };
            let dest = unsafe { core::slice::from_raw_parts_mut(base as *mut u8, size) };
            let start = sys::now_ns();
            match inflate::gunzip(packed, dest) {
                Ok(n) => {
                    log!("unpacked the file system image: {} KiB in {} ms", n >> 10, (sys::now_ns() - start) / 1_000_000);
                    image = &dest[..n];
                }
                Err(e) => {
                    log!("file system image: {}", e);
                    image = &[];
                }
            }
        }
        // The pages apps may map (system/supervisor/frames.rs): the pool less
        // the heap below and the unpacked image above. Until this, none.
        procmgr::ledger().limit(HEAP_PAGES as u32, (HEAP_PAGES + pool.app_pages) as u32);
        let mut fs = fs::Fs::new(image);
        let listed = data::load(&mut fs);
        let store = appstore::Store::load(&mut fs);
        // A saved passcode locks the phone before anything is shown (and so
        // does a /data that could not be read: it may hold one).
        trusted_ui::boot(&fs, listed);
        let book = manifest::Book::load(&mut fs);
        let app_limit = lifecycle::app_limit(MAX_APPS, untypeds.len(), pool.app_pages, RESERVE_PAGES, PER_APP_PAGES).max(1);
        log!("apps: up to {} at once ({} kernel slices, {} MiB of pool for apps)", app_limit, untypeds.len(), pool.app_pages / sys::MIB_PAGES);
        Ok(Runtime {
            app_limit,
            life: lifecycle::Book::default(),
            k,
            pool,
            fs,
            store,
            apps: core::array::from_fn(|_| None),
            home: None,
            home_tried: false,
            untypeds,
            book,
            focused: false,
            foreground: None,
            shell: device::Shell::default(),
            next_pid: 100,
            entropy: entropy::boot(),
            contact: None,
            last_poll: 0,
            stats: Stats::default(),
            audio: false,
            camera: false,
            // Radio (handset) or the emulator's host relay serves the socket pages.
            net: net::Net::new(true),
            shield: ShieldPolicy::new(),
            scorer: guard::ScoreLink::default(),
            sentinel: {
                let mut id = [0u8; 16];
                let (a, b) = (sys::counter() ^ 0x5bd1e995, sys::counter().rotate_left(17) ^ 0x9e3779b97f4a7c15);
                id[..8].copy_from_slice(&a.to_le_bytes());
                id[8..].copy_from_slice(&b.to_le_bytes());
                sentinel::Sentinel::new(crate::companion_key::COMPANION_KEY, id)
            },
            last_touch_ns: 0,
            tx_stats: (0..net::SLOTS).map(|_| None).collect(),
            last_sentinel: 0,
            wifi: wifi::Wifi::default(),
            frames: frames::Frames::default(),
            beam: scanout::Beam::default(),
            ring: touch::Ring::default(),
            tiered: None,
            // Requests made before this boot are not repeated.
            usb_serial: if cfg!(moto_board) { unsafe { (0x0900_1000 as *const u32).read_volatile() } } else { 0 },
        })
    }

    pub fn next_pid_bump(&mut self) -> u32 {
        self.next_pid += 1;
        self.next_pid - 1
    }

    /// 64 bits from the random generator (entropy.rs), for the runtime's own
    /// use. Zero if it was never seeded: then no app runs (see `start`).
    pub fn next_random(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.entropy.fill(&mut b);
        u64::from_le_bytes(b)
    }

    /// Start a program from the file system. Scripts name their interpreter
    /// on a "#!" line; a ".py" file without one runs under /bin/python3.
    pub fn launch(&mut self, path: &str, args: &[String], caller: Option<usize>) -> Result<usize, String> {
        // Loading a program can take a while; Audio and Radio keep running.
        let _lowered = sys::Lowered::new();
        let st = self.fs.stat(path).ok_or_else(|| alloc::format!("{} not found", path))?;
        let mut program = String::from(path);
        let mut argv: Vec<String> = Vec::new();
        if st.mode & fs::S_IFMT == fs::S_IFDIR {
            for main in ["main", "main.py"] {
                let p = alloc::format!("{}/{}", path, main);
                if self.fs.stat(&p).is_some() {
                    program = p;
                    break;
                }
            }
        }
        let image = self.fs.contents(&program).ok_or("not a file")?;
        let digest = sentinel::sha256(image);
        let new_hash = self.fs.from_layer(&program);
        let owner = self.shield_owner(path, &digest, caller); // Violet Shield
        let measured = program.clone();
        if image.starts_with(b"#!") {
            let line_end = image.iter().position(|&b| b == b'\n').unwrap_or(image.len());
            let line = core::str::from_utf8(&image[2..line_end]).map_err(|_| "bad #! line")?;
            let mut words = line.split_whitespace();
            let interp = String::from(words.next().ok_or("empty #! line")?);
            argv.push(interp.clone());
            argv.extend(words.map(String::from));
            argv.push(program.clone());
            program = interp;
        } else if program.ends_with(".py") {
            argv.push(String::from("/bin/python3"));
            argv.push(program.clone());
            program = String::from("/bin/python3");
        } else {
            argv.push(program.clone());
        }
        argv.extend(args.iter().cloned());
        let image = self.fs.contents(&program).ok_or_else(|| alloc::format!("{} not found", program))?;
        // The image is part of the file system, which outlives the copy below.
        let image: &'static [u8] = unsafe { core::slice::from_raw_parts(image.as_ptr(), image.len()) };

        // A new foreground app needs both an app slot and room for its startup.
        let limit = self.app_limit.min(self.untypeds.len());
        while (self.pool.available() < RESERVE_PAGES || self.apps[..limit].iter().all(Option::is_some))
            && self.reclaim(caller)
        {}
        let index = self.apps[..limit].iter().position(|a| a.is_none()).ok_or("too many apps running")?;
        // AT_RANDOM and a random address-space layout; without a seeded
        // generator nothing starts, rather than start predictably.
        let mut drawn = [0u8; 40];
        if !self.entropy.fill(&mut drawn) && !(entropy::collect(&mut self.entropy) && self.entropy.fill(&mut drawn)) {
            return Err(String::from("no entropy to start an app"));
        }
        let mut random = [0u8; 16];
        random.copy_from_slice(&drawn[..16]);
        let layout = procmgr::Layout::random(&drawn[16..]);
        let untyped = self.untypeds[index];
        let k = &mut self.k;
        let mut space = Space::new(k, untyped).map_err(|e| alloc::format!("address space: {}", e))?;
        space.cap = self.pool.app_cap(); // one app's share of the pool (pool.rs)
        let cnode = match space.object(k, Object::CNode, APP_CNODE_BITS) {
            Ok(c) => c,
            Err(e) => {
                self.release_objects(index, core::mem::take(&mut space.object_slots));
                return Err(alloc::format!("cnode: {}", e));
            }
        };
        let name = String::from(path.rsplit('/').next().unwrap_or(path));
        let cwd = String::from(match path.rfind('/') {
            Some(0) | None => "/",
            Some(i) => &path[..i],
        });
        let env: Vec<String> = [
            "HOME=/data",
            "PATH=/bin",
            "TMPDIR=/tmp",
            "LANG=C.UTF-8",
            "TERM=dumb",
            "PYTHONHOME=/",
            "PYTHONDONTWRITEBYTECODE=1",
            "PYTHONUNBUFFERED=1",
            "GODEBUG=asyncpreemptoff=1",
            "VIOLET=1",
        ]
        .iter()
        .map(|s| String::from(*s))
        .collect();
        // A dynamically linked program: musl's loader, as the system image has it.
        // (Never a copy an app wrote there: syscall.rs `loader`.)
        let interp = procmgr::interp(image).filter(|l| !self.fs.from_layer(l)).and_then(|l| self.fs.contents(l));
        let loaded = match procmgr::load(&mut space, k, &mut self.pool, image, interp, &argv, &env, random, layout) {
            Ok(l) => l,
            Err(e) => {
                space.destroy(k, &mut self.pool);
                self.release_objects(index, core::mem::take(&mut space.object_slots));
                return Err(alloc::format!("{}: {}", program, e));
            }
        };
        // ARM-mode trampolines: `ldr pc, [pc, #-4]` then the target, per thread.
        // The app has the page read-execute; the runtime writes it through its own mapping.
        space.add(layout.stubs, layout.stubs + procmgr::WINDOW, procmgr::PROT_READ | procmgr::PROT_EXEC);
        let Some(_) = space.resolve(k, &mut self.pool, layout.stubs) else {
            space.destroy(k, &mut self.pool);
            self.release_objects(index, core::mem::take(&mut space.object_slots));
            return Err(String::from("out of memory"));
        };
        let pid = self.next_pid;
        self.next_pid += 1;
        let mut app = App {
            pid,
            name,
            path: String::from(path),
            space,
            cnode,
            threads: Vec::new(),
            fds: Vec::new(),
            cwd,
            events: Default::default(),
            surface: None,
            exit_code: None,
            digest,
            new_hash,
            owner,
            frozen: None,
            used_ns: sys::now_ns(),
            life: Default::default(),
            sig: signal::Proc::new(0),
        };
        for _ in 0..3 {
            app.fds.push(Some(Rc::new(RefCell::new(device::OpenFile::new(device::Kind::Log, 2)))));
        }
        let regs = sys::Regs { sp: loaded.sp, ..Default::default() };
        self.apps[index] = Some(app);
        if let Err(e) = self.spawn(index, pid, regs, loaded.entry) {
            self.reap(index);
            return Err(alloc::format!("thread: {}", e));
        }
        log!("started {} as pid {} ({} KiB free)", program, pid, self.pool.available() * sys::PAGE_KIB);
        let app_name = self.apps[index].as_ref().unwrap().name.clone();
        self.sentinel.note_exec(pid, &app_name, &digest);
        let _ = measured;
        let line = argv.join(" ");
        match self.gate(index, &Action::SpawnApp { path: &line }) {
            Gate::Deny | Gate::Killed => {
                if let Some(app) = self.apps[index].as_mut() {
                    app.exit_code = Some(126);
                }
                self.reap(index);
                return Err(String::from("blocked by Violet Shield"));
            }
            _ => {}
        }
        Ok(index)
    }

    /// Create a thread in app `index` that starts at `target` (bit 0 selects
    /// Thumb) with `regs`. seL4 clears the Thumb bit in a written CPSR, so the
    /// thread starts at its ARM-mode trampoline, which jumps there.
    pub fn spawn(&mut self, index: usize, tid: u32, mut regs: sys::Regs, target: u32) -> Result<usize, i32> {
        let app = self.apps[index].as_mut().unwrap();
        // A thread that ended (exit, a vfork child after execve) leaves its
        // slot, TCB and fault endpoint for the next: posix_spawn makes and
        // ends one each time, and slots and kernel memory are never freed.
        let reuse = app.threads.iter().position(|t| !t.alive && t.reply.is_none());
        let slot = reuse.unwrap_or(app.threads.len());
        // The app's CNode holds one badged fault endpoint per thread.
        if slot >= (1 << APP_CNODE_BITS) - 2 {
            return Err(-11);
        }
        let k = &mut self.k;
        if let Some(s) = reuse {
            let (tcb, sc) = (app.threads[s].tcb, app.threads[s].sc);
            if sys::mcs() || sys::cores() > 1 {
                let tier = if self.tiered == Some(index) { sys::tier_foreground() } else { sys::tier_background(index) };
                sys::tcb_schedule(tcb, sc, 0, tier)?;
            }
            let stub = app.space.stub(k, &mut self.pool, slot, target).ok_or(-12)?;
            (regs.pc, regs.cpsr) = (stub, 0x10);
            let t = &mut app.threads[s];
            (t.tid, t.wait, t.deadline, t.saved, t.clear_tid, t.alive) = (tid, None, None, [0; 9], 0, true);
            t.sig = signal::Thread::new(0);
            sys::tcb_write(tcb, true, &regs)?;
            return Ok(slot);
        }
        let tcb = app.space.object(k, Object::Tcb, 0)?;
        let ep_slot = slot as u32 + 1;
        sys::mint_into(app.cnode, ep_slot, APP_CNODE_BITS, sys::SLOT_FAULT_EP, badge(index, slot))?;
        sys::tcb_configure(tcb, ep_slot, app.cnode, APP_CNODE_BITS, app.space.pd)?;
        // MCS: its own scheduling context, from the app's kernel memory, and
        // the fault endpoint named in this CSpace.
        let mut sc = 0;
        if sys::mcs() {
            sc = app.space.object(k, Object::SchedContext, sys::SCHED_CONTEXT_BITS)?;
            let fault = k.slot();
            sys::mint_into(sys::SLOT_CNODE, fault, sys::CNODE_BITS, sys::SLOT_FAULT_EP, badge(index, slot))?;
            let tier = if self.tiered == Some(index) { sys::tier_foreground() } else { sys::tier_background(index) };
            let r = sys::tcb_schedule(tcb, sc, fault, tier);
            sys::delete(fault);
            k.release(fault);
            r?;
        } else if sys::cores() > 1 {
            // Without MCS: just the core.
            let tier = if self.tiered == Some(index) { sys::tier_foreground() } else { sys::tier_background(index) };
            sys::tcb_schedule(tcb, 0, 0, tier)?;
        }
        // The trampoline page is looked up each time, never through a kept
        // address: no thread starts if the runtime's page is not there.
        let stub = app.space.stub(k, &mut self.pool, slot, target).ok_or(-12)?;
        regs.pc = stub;
        regs.cpsr = 0x10;
        app.threads.push(Thread {
            tid,
            tcb,
            reply: None,
            wait: None,
            deadline: None,
            saved: [0; 9],
            clear_tid: 0,
            alive: true,
            slot: slot as u32,
            sc,
            sig: signal::Thread::new(0),
        });
        sys::tcb_write(tcb, true, &regs)?;
        Ok(slot)
    }

    fn release_objects(&mut self, index: usize, slots: Vec<u32>) {
        // Revoking the app's kernel memory deletes every object made from it.
        sys::revoke(self.untypeds[index]);
        for slot in slots {
            self.k.release(slot);
        }
    }

    /// Reclaim an app slot and memory: close a background app, frozen ones
    /// first, then cached, then Settings, then visible ones, least recently
    /// used first and an app with an audio stream last (lifecycle.rs).
    fn reclaim(&mut self, caller: Option<usize>) -> bool {
        self.update_pressure(sys::now_ns());
        let Some(v) = self.victim(caller, 3) else { return false };
        self.close_for_memory(v, "low memory");
        true
    }

    /// Tear an app down: its threads stop and its memory returns to the pool.
    pub fn reap(&mut self, index: usize) {
        let Some(mut app) = self.apps[index].take() else { return };
        // Its parent learns its wait status (syscall.rs, processes).
        self.proc_ended(&app);
        // Its wake lock, if it held one, ends with it.
        powerctl::app_gone(app.pid);
        // The resolver forgets the names it resolved (names.rs).
        self.net.forget(app.pid);
        for t in &app.threads {
            if t.alive {
                sys::tcb_suspend(t.tcb);
            }
            if let Some(r) = t.reply {
                sys::delete(r);
                self.k.release(r);
            }
        }
        // Its page directory goes first (with its kernel objects), so giving
        // back each page costs no page-table walk or TLB flush of its own.
        self.release_objects(index, core::mem::take(&mut app.space.object_slots));
        app.space.destroy(&mut self.k, &mut self.pool);
        // Screen pages only now, when no mapping holds them (frames.rs).
        if let Some(surface) = app.surface.take() {
            for p in surface.pages {
                self.pool.give_back(p);
            }
        }
        log!("{} (pid {}) exited with status {}", app.name, app.pid, app.exit_code.unwrap_or(-1));
        if self.home == Some(index) {
            self.home = None;
        }
        if app.frozen.is_some() {
            self.sentinel.frozen_apps = self.sentinel.frozen_apps.saturating_sub(1);
        }
        let status = alloc::format!("status {}", app.exit_code.unwrap_or(-1));
        let ev = TelemetryEvent::new(sys::now_ns(), AppId(app.pid), EventKind::Exit, 0, status.as_bytes());
        self.shield.ring.emit(ev);
        if self.foreground == Some(index) {
            self.foreground = None;
            self.show_home();
        }
    }

    /// Answer a waiting thread and let it run again.
    /// While its app is frozen (lifecycle or Shield) the answer is held
    /// (Wait::Held) and given when it thaws: a frozen app runs no code.
    pub fn wake(&mut self, index: usize, slot: usize, result: i32) {
        let Some(app) = self.apps[index].as_mut() else { return };
        let held = lifecycle::held(app.frozen.is_some(), &app.life);
        let t = &mut app.threads[slot];
        if held && t.reply.is_some() {
            t.wait = Some(Wait::Held { result });
            t.deadline = None;
            return;
        }
        let Some(reply) = t.reply.take() else { return };
        t.wait = None;
        t.deadline = None;
        let mut mrs = t.saved;
        if result == trusted_ui::RESTART {
            // Back onto the svc (saved[8] is past it: 4 bytes in ARM, 2 in Thumb): the call runs again as made.
            let thumb = sys::tcb_read(t.tcb).is_ok_and(|r| signal::thumb(r.cpsr));
            mrs[8] = mrs[8].wrapping_sub(if thumb { 2 } else { 4 });
        } else {
            mrs[0] = result as u32;
        }
        sys::send(reply, &mrs);
        self.k.reply_done(reply);
    }

    fn handle_fault(&mut self, r: sys::Received) {
        let index = (r.badge >> 12) as usize;
        let slot = ((r.badge & 0xfff) - 1) as usize;
        if index >= MAX_APPS || self.apps[index].as_ref().map_or(true, |a| slot >= a.threads.len()) {
            return;
        }
        // A fault in the runtime from here on closes this app only (contain.rs);
        // the callers say when the work is over.
        contain::STATE.serve(index);
        // Test images only (built with VIOLET_TEST_FAULT=1, never a release):
        // system call 0xffa17 makes the runtime fault while it serves the
        // caller, by a panic (r0 = 0) or a write to the unmapped page at 0.
        #[cfg(open_fault_probe)]
        if r.label == sys::FAULT_UNKNOWN_SYSCALL && r.mrs[7] == 0xf_fa17 {
            if r.mrs[0] == 0 {
                panic!("fault probe");
            }
            unsafe { ((r.mrs[0] & 0xffc) as usize as *mut u32).write_volatile(0) };
        }
        match r.label {
            sys::FAULT_UNKNOWN_SYSCALL => self.syscall(index, slot, &r.mrs),
            sys::FAULT_VM => {
                let (pc, addr) = (r.mrs[0], r.mrs[1]);
                // Read, write or instruction fetch: the region must allow it (vma.rs).
                let access = vma::access(r.mrs[2], r.mrs[3]);
                let mut resolved = self.apps[index].as_mut().unwrap().space.fault(&mut self.k, &mut self.pool, addr, access);
                // Out of pages: close an app that matters less and try again.
                // The app in front, Home or the agent may close any app
                // behind; another app only frozen and cached ones, then
                // itself (lifecycle.rs `fault_victim`).
                let valid = self.apps[index].as_ref().unwrap().space.find(addr).is_some_and(|v| access.allowed(v.prot));
                if !resolved && valid && self.pool.available() == 0 {
                    self.update_pressure(sys::now_ns());
                }
                while !resolved && valid && self.pool.available() == 0 {
                    match self.fault_victim_for(index) {
                        Some(v) if v != index => self.close_for_memory(v, "low memory"),
                        _ => break,
                    }
                    resolved = self.apps[index].as_mut().unwrap().space.fault(&mut self.k, &mut self.pool, addr, access);
                }
                // Any fault a region allows that still has no page ran out
                // of memory (the pool, its family's cap: pool.rs `app_cap`,
                // kernel memory or slots): out of memory, never a bad access
                // (procmgr.rs `starved`).
                if !resolved && valid && !self.apps[index].as_ref().unwrap().space.holds(addr) {
                    return self.close_for_memory(index, "out of memory");
                }
                if resolved {
                    sys::reply(&[]);
                    // A fault that waited while its app froze: the thread stays put until it thaws.
                    if self.apps[index].as_ref().is_some_and(|a| a.frozen.is_none() && a.life.state == lifecycle::State::Frozen) {
                        self.hold_thread(index, slot);
                    }
                } else if self.fault_signal(index, slot, r.label, addr, Some(access), r.mrs[3]) {
                    // A SIGSEGV handler runs (syscall.rs, signals).
                } else {
                    let app = self.apps[index].as_mut().unwrap();
                    let (pages, tables) = app.space.usage();
                    log!("{} (pid {}): bad memory access ({:?}) at {:#x}, pc {:#x} ({} KiB free; it held {} KiB in {} tables)", app.name, app.pid, access, addr, pc, self.pool.available() * sys::PAGE_KIB, pages * sys::PAGE_KIB, tables);
                    app.exit_code = Some(139);
                    self.reap(index);
                }
            }
            // An undefined instruction: SIGILL, to a handler if there is one.
            sys::FAULT_USER_EXCEPTION if self.fault_signal(index, slot, r.label, r.mrs[0], None, 0) => {}
            _ => {
                let app = self.apps[index].as_mut().unwrap();
                log!("{} (pid {}): fault {} at pc {:#x}", app.name, app.pid, r.label, r.mrs[0]);
                app.exit_code = Some(132);
                self.reap(index);
            }
        }
    }

    /// Wake threads whose wait finished or timed out.
    fn check_waiters(&mut self) {
        let now = sys::now_ns();
        for index in 0..MAX_APPS {
            let Some(app) = self.apps[index].as_ref() else { continue };
            // Neither a Shield freeze nor a lifecycle freeze serves waits.
            if app.frozen.is_some() || app.life.state == lifecycle::State::Frozen {
                continue;
            }
            contain::STATE.serve(index); // finishing this app's waits is work for it
            let waiting: Vec<(usize, Wait, Option<u64>)> = app
                .threads
                .iter()
                .enumerate()
                .filter_map(|(i, t)| t.wait.clone().map(|w| (i, w, t.deadline)))
                .collect();
            for (slot, wait, deadline) in waiting {
                if let Some(result) = self.ready(index, &wait) {
                    self.wake(index, slot, result);
                } else if deadline.map_or(false, |d| now >= d) {
                    let result = match wait {
                        Wait::Futex { .. } => syscall::ETIMEDOUT,
                        Wait::Poll { .. } | Wait::Epoll { .. } | Wait::Sleep => 0,
                        Wait::Read { .. } | Wait::RecvFrom { .. } | Wait::Audio { .. } | Wait::Send { .. } => syscall::EAGAIN,
                        Wait::Accept { .. } => syscall::EAGAIN,
                        Wait::Connect { .. } => -110,
                        Wait::Frozen | Wait::Consent { .. } => syscall::EACCES,
                        Wait::Present { result } | Wait::Held { result } => result,
                        Wait::Vsync { .. } => 0,
                        Wait::Signal => syscall::EAGAIN,
                    };
                    self.wake(index, slot, result);
                }
            }
        }
        contain::STATE.done();
    }

    /// After a fault in this process (a panic, a bad pointer) the supervisor
    /// started the thread again and `run` found the runtime as the fault left
    /// it (contain.rs). The app being served is closed; the others go on.
    fn recover(&mut self, served: Option<usize>) {
        // What the lost stack would have undone: a lowered priority
        // (sys::Lowered), and with MCS a Reply object taken for a saved call:
        // made from the untyped kept for Replies (pool.rs `Kernel::replies`;
        // SLOT_KERNEL is carved up by then). The retype fails harmlessly
        // when the slot still holds one.
        if sys::timer_ready() {
            sys::set_own_priority(sys::EVENT_PRIORITY);
        }
        if sys::mcs() {
            let from = if self.k.replies != 0 { self.k.replies } else { sys::SLOT_KERNEL };
            let _ = sys::retype(from, Object::Reply, 0, sys::SLOT_REPLY);
        }
        let n = contain::STATE.recovered();
        if contain::recover(self, served) == contain::Verdict::Nothing {
            log!("runtime fault {} in no app's call: stopped", n);
            halt()
        }
    }

    /// On the handset, USB Diagnostics can ask for a program to be started.
    #[cfg(moto_board)]
    fn usb_requests(&mut self) {
        let page = 0x0900_1000 as *const u32;
        let serial = unsafe { page.read_volatile() };
        if serial == self.usb_serial {
            return;
        }
        self.usb_serial = serial;
        let len = (unsafe { page.add(1).read_volatile() } as usize).min(40);
        let mut bytes = [0u8; 40];
        for (i, b) in bytes[..len].iter_mut().enumerate() {
            *b = unsafe { (0x0900_1008 as *const u8).add(i).read_volatile() };
        }
        let path = String::from_utf8_lossy(&bytes[..len]).into_owned();
        if let Some(args) = path.strip_prefix("@shot") {
            self.usb_shot(args);
            return;
        }
        // The panel's scan line, sampled for 200 ms (frames seen, period).
        // "@sys frames" or "@sys bench tap X Y N": measurements, answered in the log.
        if let Some(line) = path.strip_prefix("@sys ") {
            // Shared settings too ("@sys set relay 192.168.1.20:8787" points the agent at a relay).
            if line.starts_with("frames") || line.starts_with("bench ") || line.starts_with("set ") || line.starts_with("get ") || line == "installed" || line == "memory" {
                // The trusted UI knows it is the cable, not the app in front.
                trusted_ui::set_usb(true);
                // "memory" over the cable is every app's, whatever app is in front.
                let answer = if line == "memory" { self.lifecycle_report(None) + "\n" } else { self.system(self.foreground.unwrap_or(0), line) };
                trusted_ui::set_usb(false);
                log!("{}: {}", line, answer.trim_end());
            }
            return;
        }
        // Nothing is started on a locked phone (trusted_ui.rs).
        if trusted_ui::locked() {
            log!("locked: not starting {}", path);
            return;
        }
        // A new build replaces the copy that is running.
        for i in 0..MAX_APPS {
            if self.apps[i].as_ref().map_or(false, |a| a.path == path) {
                log!("replacing the running {}", path);
                self.reap(i);
            }
        }
        match self.launch(&path, &[], None) {
            Ok(i) => {
                if path == HOME_APP {
                    self.home = Some(i);
                }
                self.set_foreground(Some(i));
                self.shell.dirty = true;
            }
            Err(e) => log!("could not start {}: {}", path, e),
        }
    }
    #[cfg(not(moto_board))]
    fn usb_requests(&mut self) {}

    /// A screenshot over USB (handset), one strip per request: "@shot STRIP SCALE NONCE"
    /// copies screen rows, scaled down by SCALE, as RGB565 into the 64 KiB install
    /// window after a 32-byte header ("SHOT", nonce, strip, scale, width, height,
    /// rows per strip, rows here). The nonce is written last, so the host knows
    /// when the strip is complete.
    #[cfg(moto_board)]
    fn usb_shot(&self, args: &str) {
        let mut words = args.split_whitespace().map(|w| w.parse::<usize>().unwrap_or(0));
        let (strip, scale, nonce) = (words.next().unwrap_or(0), words.next().unwrap_or(2).clamp(1, 8), words.next().unwrap_or(0));
        let (w, h) = (device::WIDTH / scale, device::HEIGHT / scale);
        let per = (65536 - 32) / (w * 2);
        let (y0, y1) = ((strip * per).min(h), (strip * per + per).min(h));
        let window = 0x0950_1000 as *mut u8;
        let mut at = 32;
        for y in y0..y1 {
            let row = (sys::FRAMEBUFFER + y * scale * device::STRIDE) as *const u32;
            for x in 0..w {
                let p = unsafe { row.add(x * scale).read_volatile() };
                let v = (((p >> 19) & 0x1f) << 11 | ((p >> 10) & 0x3f) << 5 | ((p >> 3) & 0x1f)) as u16;
                unsafe {
                    window.add(at).write_volatile(v as u8);
                    window.add(at + 1).write_volatile((v >> 8) as u8);
                }
                at += 2;
            }
        }
        let head = window as *mut u32;
        let fields = [u32::from_le_bytes(*b"SHOT"), 0, strip as u32, scale as u32, w as u32, h as u32, per as u32, (y1 - y0) as u32];
        unsafe {
            for (i, f) in fields.iter().enumerate().skip(2) {
                head.add(i).write_volatile(*f);
            }
            head.write_volatile(fields[0]);
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            head.add(1).write_volatile(nonce as u32);
        }
    }

    /// Installs over USB (handset): append the 64 KiB window to /apps/NAME.
    /// Control words: 0 commit serial (USB), 1 done serial, 2 result; file
    /// offset at byte 16, length at 20, the name from byte 24.
    #[cfg(moto_board)]
    fn usb_installs(&mut self) {
        let control = 0x0950_0000 as *mut u32;
        let (commit, done) = unsafe { (control.read_volatile(), control.add(1).read_volatile()) };
        if commit == done {
            return;
        }
        let _lowered = sys::Lowered::new();
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let (offset, len) = unsafe { (control.add(4).read_volatile() as usize, control.add(5).read_volatile() as usize) };
        let mut name = [0u8; 32];
        for (i, b) in name.iter_mut().enumerate() {
            *b = unsafe { (0x0950_0018 as *const u8).add(i).read_volatile() };
        }
        let end = name.iter().position(|&b| b == 0).unwrap_or(32);
        let name = String::from_utf8_lossy(&name[..end]).into_owned();
        let path = alloc::format!("/apps/{}", name);
        // Blocks go to a hidden staging file; an empty block at the end swaps
        // it into place, so a push that fails leaves the working copy alone.
        let staged = alloc::format!("/apps/.{}.part", name);
        let data = unsafe { core::slice::from_raw_parts(0x0950_1000 as *const u8, len.min(65536)) };
        let valid = !name.is_empty() && !name.contains('/') && !name.starts_with('.');
        if valid && len == 0 && offset > 0 {
            let ok = self.fs.contents(&staged).map_or(false, |d| d.len() == offset) && self.fs.rename(&staged, &path).is_ok();
            if ok {
                log!("installed {} ({} bytes)", path, offset);
                self.shell.dirty = true;
            }
            unsafe {
                control.add(2).write_volatile(if ok { 0 } else { 1 });
                core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
                control.add(1).write_volatile(commit);
            }
            return;
        }
        let ok = valid && match self.fs.writable(&staged, true, offset == 0, 0) {
            Some(file) => {
                if file.len() < offset {
                    false
                } else {
                    file.truncate(offset);
                    // Grow in 1 MiB steps rather than doubling, and fail the
                    // install (not the runtime) when the heap is full.
                    let want = file.len() + data.len();
                    let step = if want > file.capacity() { (want - file.len()).max(1 << 20) } else { 0 };
                    if file.try_reserve_exact(step).is_err() {
                        log!("not enough memory to install {}", path);
                        false
                    } else {
                        file.extend_from_slice(data);
                        true
                    }
                }
            }
            None => false,
        };
        if ok && offset == 0 {
            log!("installing {}", path);
        }
        unsafe {
            control.add(2).write_volatile(if ok { 0 } else { 1 });
            core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
            control.add(1).write_volatile(commit);
        }
    }
    #[cfg(not(moto_board))]
    fn usb_installs(&mut self) {}

    fn poll_runtime(&mut self) {
        let t0 = sys::now_ns();
        data::flush(&mut self.fs);
        self.wifi.tick(&self.fs, t0);
        // Bluetooth sharing (hci.rs): its own clean-up after an app closed.
        hci::tick();
        // Network time (timesync.rs).
        // Each request draws its own 64 bits; none if the generator is unseeded.
        let entropy = &mut self.entropy;
        timesync::tick(&mut self.net, t0, &mut || {
            let mut b = [0u8; 8];
            entropy.fill(&mut b).then(|| u64::from_le_bytes(b))
        });
        self.usb_requests();
        self.usb_installs();
        let t1 = sys::now_ns();
        // With the poll, whether the lock screen is up: Runtime then keeps the
        // open apps in front, so no built-in screen is reachable behind it.
        let focus = request(GET, FOCUS, trusted_ui::locked() as u32);
        let t2 = sys::now_ns();
        self.stats.step(0, t1 - t0);
        self.stats.step(1, t2 - t1);
        let focused = focus.op == OK && focus.value == OPEN_APPS;
        // Runtime answers 0 while the phone sleeps: the screen is off, so lock.
        if focus.op == OK && focus.value == 0 {
            trusted_ui::screen_off();
        }
        if focused != self.focused {
            log!("in front: {} (focus {} op {})", focused, focus.value, focus.op);
        }
        if focused && !self.focused && !self.home_tried {
            // Home starts the first time the open apps come to the front, so it
            // costs nothing on a system that never shows them.
            self.home_tried = true;
            self.start_home(true);
        }
        if focused && !self.focused {
            self.shell.dirty = true;
            self.shell.armed = false;
            if self.ring.live() {
                // Interrupt-driven touch sends no idle samples to arm the
                // shell; a finger already lifted counts as lifted.
                self.shell.arm_with(self.ring.down);
            }
            // Not under system UI: the app hears FOCUS_GAINED when it goes (trusted_ui.rs).
            if let Some(i) = self.foreground.filter(|_| !trusted_ui::shown_over()) {
                self.refresh(i);
                self.lifecycle(i, abi::LIFECYCLE_FOCUS_GAINED);
            }
        }
        if !focused && self.focused {
            // Under system UI it was told already (trusted_ui.rs ui_switch).
            if let Some(i) = self.foreground.filter(|_| !trusted_ui::shown_over()) {
                self.lifecycle(i, abi::LIFECYCLE_FOCUS_LOST);
            }
        }
        self.focused = focused;
        if !focused {
            self.frames.last_poll_ns = 0;
            return;
        }
        self.frames.polled(t0);
        if self.contact.is_none() {
            let r = Message::new(SUBSCRIBE, CONTACT, 0).call(sys::SLOT_RUNTIME);
            if r.op == OK {
                self.contact = Some(r.value);
            }
        }
        // Touch comes from Touch's interrupt ring; poll Runtime only if the
        // ring is not running, or to confirm a release that never arrived.
        // Still ask every 500 ms: Touch kicks the hardware watchdog when it
        // serves a request, so this chain (Runtime, its worker, Touch) is
        // also how the phone knows the system is alive.
        let stuck = self.ring.down && t0.wrapping_sub(self.ring.last_ns) > 200_000_000;
        let liveness = t0.wrapping_sub(self.ring.polled_ns) > 500_000_000;
        let ring_ok = self.ring.live() && !stuck && !liveness;
        if !ring_ok && self.ring.live() {
            self.ring.polled_ns = t0;
        }
        if let Some(stream) = self.contact.filter(|_| !ring_ok) {
            let t = sys::now_ns();
            // Runtime answers a touch poll with the sample its worker read
            // last and starts the next read; give that worker one turn so
            // this poll delivers the current sample, not one a poll old.
            let mut waited = false;
            for _ in 0..8 {
                let e = Message::new(POLL, 0, stream).call(sys::SLOT_RUNTIME);
                if e.op != OK {
                    if waited {
                        break;
                    }
                    waited = true;
                    sys::pause();
                    continue;
                }
                self.touch(e.value as i32, e.arg as i32, e.key != 0);
            }
            self.stats.step(2, sys::now_ns() - t);
        }
        if self.foreground.is_none() && self.shell.dirty {
            self.draw_shell();
        }
    }

    /// MCS priority tiers (docs/8ms-frame-pipeline-rfc.md): the app on screen
    /// runs at 120 with 6 ms of every 8, ahead of the services at 100 but
    /// never starving them; every other app runs round robin at 100.
    fn set_tiers(&mut self) {
        let front = self.foreground.filter(|_| self.focused);
        if front == self.tiered || (!sys::mcs() && sys::cores() == 1) {
            return;
        }
        for (i, front) in [(self.tiered, false), (front, true)] {
            let Some(app) = i.and_then(|i| self.apps[i].as_ref()) else { continue };
            let tier = if front { sys::tier_foreground() } else { sys::tier_background(i.unwrap()) };
            for t in app.threads.iter().filter(|t| t.alive && (t.sc != 0 || !sys::mcs())) {
                let _ = sys::tcb_schedule(t.tcb, t.sc, 0, tier);
            }
        }
        self.tiered = front;
    }

    /// Runtime polls (focus, and touch without the interrupt ring): every
    /// 4 ms while apps run and a finger is down or lifted under a second ago,
    /// 16 ms otherwise or when touch arrives by interrupt, 50 ms idle. (The
    /// first press after a quiet second waits up to 16 ms, like the ring's.)
    fn poll_interval(&self, busy: bool) -> u64 {
        // Every sample with a finger down moves last_touch_ns (device.rs).
        let touching = self.last_poll.wrapping_sub(self.last_touch_ns) < 1_000_000_000;
        match (busy, self.ring.live() || !touching) {
            (false, _) => 50_000_000,
            (true, true) => 16_000_000,
            (true, false) => 4_000_000,
        }
    }

    /// Reports Touch has put in the ring since the last pass, to the app in front.
    fn drain_touch(&mut self) {
        while let Some((x, y, down, delay_ns)) = self.ring.next() {
            if self.focused {
                self.frames.touch_irq.add(delay_ns);
                self.touch(x, y, down);
            }
        }
    }

    /// Synthetic presses from `bench tap`: a touch down on the app in front,
    /// then, 150 ms later, focus handed back so it drops the press.
    fn bench_tick(&mut self, now: u64) {
        let Some(mut b) = self.frames.bench else { return };
        // Never onto system UI, and never under it (trusted_ui.rs).
        if trusted_ui::covers() {
            self.frames.bench = None;
            return;
        }
        if now < b.next_ns {
            return;
        }
        let Some(i) = self.foreground.filter(|_| self.focused) else {
            // `bench press` on the app list taps it like a finger would.
            if b.release && self.focused && self.foreground.is_none() {
                self.synthetic_touch(b.x, b.y, !b.pressed); // not the user, to Violet Shield
                b.pressed = !b.pressed;
                if !b.pressed {
                    b.left -= 1;
                }
                b.next_ns = now + if b.pressed { 120_000_000 } else { 600_000_000 };
                self.frames.bench = (b.left > 0).then_some(b);
                return;
            }
            self.frames.bench = None;
            return;
        };
        let touch = |kind, y| abi::InputEvent { timestamp_us: now / 1000, kind, code: 0, x: b.x, y, value: 0 };
        if b.pressed && b.dy != 0 {
            // A drag: a move every 8 ms, 250 ms in all, then the finger lifts.
            b.step += 1;
            let y = b.y + b.dy * b.step.min(31) / 31;
            if b.step <= 31 {
                self.event(i, touch(abi::EVENT_TOUCH_MOVE, y));
                b.next_ns = now + 8_000_000;
            } else {
                self.event(i, touch(abi::EVENT_TOUCH_UP, y));
                b.pressed = false;
                b.step = 0;
                b.left -= 1;
                b.next_ns = now + 1_500_000_000;
            }
        } else if b.pressed && b.release {
            self.event(i, touch(abi::EVENT_TOUCH_UP, b.y));
            b.pressed = false;
            b.left -= 1;
            b.next_ns = now + 600_000_000;
        } else if b.pressed {
            self.lifecycle(i, abi::LIFECYCLE_FOCUS_GAINED);
            b.pressed = false;
            b.left -= 1;
            b.next_ns = now + 350_000_000;
        } else {
            self.event(i, touch(abi::EVENT_TOUCH_DOWN, b.y));
            b.pressed = true;
            b.next_ns = now + if b.dy != 0 { 8_000_000 } else { 150_000_000 };
        }
        self.frames.bench = (b.left > 0).then_some(b);
    }

    fn run(&mut self) -> ! {
        // Since when the loop has had calls to serve on every pass (cores.rs).
        let mut busy_since = 0u64;
        loop {
            let mut handled = 0;
            while handled < 64 {
                let r = sys::receive(false);
                if r.badge == 0 {
                    break;
                }
                if r.badge & sys::NOTIFY_BADGES != 0 {
                    if r.badge & sys::TIMER_BADGE != 0 {
                        sys::timer_disarm(); // an expiry that came while we worked
                    }
                    continue;
                }
                self.handle_fault(r);
                contain::STATE.done();
                handled += 1;
            }
            // Timers, and signals for threads not in a call (syscall.rs): with
            // fewer than 64 handled, the fault queue was found empty.
            self.signal_pass(handled < 64);
            let now = sys::now_ns();
            self.stats.loops += 1;
            self.stats.faults += handled as u64;
            if self.stats.since == 0 {
                self.stats.since = now;
            }
            if now.wrapping_sub(self.stats.since) >= 30_000_000_000 && self.apps.iter().any(|a| a.is_some()) {
                let secs = (now - self.stats.since) / 1_000_000_000;
                let (blocks, free) = heap::HEAP.stats();
                let waiting: usize = self.apps.iter().flatten().map(|a| a.threads.iter().filter(|t| t.wait.is_some()).count()).sum();
                let threads: usize = self.apps.iter().flatten().map(|a| a.threads.iter().filter(|t| t.alive).count()).sum();
                // The four busiest calls, picked by hand: a sort here costs kilobytes of code.
                let mut calls = core::mem::take(&mut self.stats.calls);
                let mut top = String::new();
                for _ in 0..4 {
                    let mut best = 0;
                    for j in 1..calls.len() {
                        if calls[j].1 > calls[best].1 {
                            best = j;
                        }
                    }
                    let Some(c) = calls.get_mut(best).filter(|c| c.1 != 0) else { break };
                    top += &alloc::format!("{}:{}/s ", c.0, c.1 as u64 / secs);
                    c.1 = 0;
                }
                log!(
                    "stats: {} loops/s, asleep {}%, {} faults/s [{}], slowest poll {} us (flush {} focus {} touch {}), slowest pass {} us, {} threads ({} waiting), heap {} free blocks {} KiB",
                    self.stats.loops / secs, self.stats.slept_ns / 10_000_000 / secs, self.stats.faults / secs, top.trim_end(), self.stats.slowest_poll / 1000,
                    self.stats.slowest_step[0] / 1000, self.stats.slowest_step[1] / 1000, self.stats.slowest_step[2] / 1000,
                    self.stats.slowest_pass / 1000, threads, waiting, blocks, free / 1024
                );
                self.stats = Stats { since: now, ..Stats::default() };
                if self.frames.present.count > 0 && self.frames.bench.is_none() {
                    let report = self.frames.report(now);
                    log!("frames: {}", report.trim_end());
                }
            }
            if self.stats.pass_start != 0 {
                self.stats.slowest_pass = self.stats.slowest_pass.max(now.wrapping_sub(self.stats.pass_start));
            }
            self.stats.pass_start = now;
            // Poll Runtime often while an app runs or the app list is on screen;
            // otherwise rarely, so an idle Open Runtime costs the other processes little.
            let busy = self.focused || self.apps.iter().any(|a| a.is_some());
            let interval = self.poll_interval(busy);
            if now.wrapping_sub(self.last_poll) >= interval {
                self.last_poll = now;
                self.poll_runtime();
                self.stats.slowest_poll = self.stats.slowest_poll.max(sys::now_ns().wrapping_sub(now));
            }
            self.drain_touch();
            // The band, prompts, the passcode pad and the lock screen.
            self.ui_tick(now);
            self.set_tiers();
            self.bench_tick(now);
            self.beam.tick();
            self.flush_presents();
            // Before the waiters: an app back in front runs again this pass.
            self.lifecycle_tick(now);
            net::linger_tick(now);
            self.check_waiters();
            if now.wrapping_sub(self.last_sentinel) >= 100_000_000 {
                self.last_sentinel = now;
                self.shield_tick(now);
            }
            if handled == 0 {
                busy_since = 0;
                self.idle(now);
            } else if busy_since == 0 {
                busy_since = now;
            } else if cores::relief_due(sys::cores(), busy_since, now) {
                // An app on another core can keep this loop busy, above the
                // services on their own core: let them run once.
                busy_since = 0;
                sys::relieve();
            }
        }
    }

    /// Nothing left to do this pass. With the timer, sleep until the next
    /// Runtime poll, thread deadline or frame, or until an app calls; at
    /// EVENT_PRIORITY the wake-up runs ahead of the polling services. Without
    /// it, yield as before.
    fn idle(&mut self, now: u64) {
        if !sys::timer_ready() {
            return sys::yield_now();
        }
        let sleep = self.next_wake(now).saturating_sub(sys::now_ns());
        if sleep < 50_000 {
            return;
        }
        sys::timer_arm(sleep);
        let r = sys::receive(true);
        sys::timer_disarm();
        let woke = sys::now_ns();
        self.stats.slept_ns += woke - now;
        self.stats.pass_start = 0; // a pass measures work, not sleep
        if r.badge != 0 && r.badge & sys::NOTIFY_BADGES == 0 {
            self.handle_fault(r);
            contain::STATE.done();
        }
    }

    /// When the loop must run next: the Runtime poll (touch, focus), the
    /// Shield tick, thread deadlines, presents waiting on the panel, frames
    /// for VSync waiters, and a short interval while a thread waits on
    /// something only polling notices (sockets, the audio ring, the
    /// microphone, camera or Bluetooth, directly or through poll/epoll). A
    /// wait on events, pipes or /dev/violet/sys needs no polling: only this
    /// loop changes them, and it checks the waiters after every pass. Frozen
    /// apps wake nothing.
    fn next_wake(&mut self, now: u64) -> u64 {
        let busy = self.focused || self.apps.iter().any(|a| a.is_some());
        let mut wake = self.last_poll + self.poll_interval(busy);
        wake = wake.min(self.last_sentinel + 100_000_000);
        if let Some(b) = self.frames.bench {
            wake = wake.min(b.next_ns);
        }
        let next_frame = self.beam.frame_ns + u64::from(self.beam.period_us()) * 1000 + 200_000;
        for app in self.apps.iter().flatten().filter(|a| a.life.state != lifecycle::State::Frozen) {
            if let Some(([_, y0, _, y1], _)) = app.surface.as_ref().and_then(|s| s.pending) {
                // Just as the beam leaves the rows.
                wake = wake.min(now + self.beam.wait_ns(y0, y1).max(50_000));
            }
            for t in app.threads.iter().filter(|t| t.alive) {
                let Some(w) = &t.wait else { continue };
                if let Some(d) = t.deadline {
                    wake = wake.min(d);
                }
                match w {
                    Wait::Futex { .. } | Wait::Sleep | Wait::Frozen | Wait::Present { .. } | Wait::Signal | Wait::Consent { .. } => {}
                    Wait::Vsync { .. } => wake = wake.min(next_frame.max(now + 500_000)),
                    w if !polled(app, w) => {}
                    _ => wake = wake.min(now + 2_000_000),
                }
            }
        }
        if let Some(t) = self.signal_wake(now) {
            wake = wake.min(t); // signal timers (syscall.rs)
        }
        wake.min(now + 50_000_000)
    }
}

// ---- Background lifecycle and the low-memory killer (policy: lifecycle.rs) ----

/// Whether fd's readiness can change without this loop doing it (a socket,
/// the audio or microphone ring, camera frames, Bluetooth), so a wait on it
/// needs polling. Unknown kinds count as such.
fn external(app: &App, fd: i32) -> bool {
    use device::Kind::*;
    app.fds.get(fd as usize).and_then(|f| f.as_ref()).is_some_and(|f| {
        !matches!(f.borrow().kind, Events | Sys { .. } | EventFd { .. } | Pipe { .. } | File { .. } | Dir { .. } | Log | Null | Random | Screen)
    })
}

const _: () = assert!(abi::LIFECYCLE_LOW_MEMORY == lifecycle::LIFECYCLE_LOW_MEMORY);

/// Bytes waiting in the pipes `app` writes to, each pipe once: runtime heap
/// it holds (lifecycle.rs `pipe_room`, `heap_victim`).
pub fn pipe_bytes(app: &App) -> usize {
    let mut ends: Vec<(usize, usize)> = app.fds.iter().flatten().filter_map(|f| match &f.borrow().kind {
        device::Kind::Pipe { buffer, write: true, .. } => Some((Rc::as_ptr(buffer) as usize, buffer.borrow().len())),
        _ => None,
    }).collect();
    ends.sort_unstable();
    ends.dedup_by_key(|e| e.0);
    ends.iter().map(|e| e.1).sum()
}

/// Whether a wait needs the loop's short polling interval.
fn polled(app: &App, w: &Wait) -> bool {
    match w {
        Wait::Read { fd, .. } => external(app, *fd),
        Wait::Poll { fds, .. } => fds.iter().any(|&(fd, _)| external(app, fd)),
        Wait::Epoll { epfd, .. } => app.fds.get(*epfd as usize).and_then(|f| f.as_ref()).map_or(true, |f| match &f.borrow().kind {
            device::Kind::Epoll { interest, .. } => interest.iter().any(|&(fd, _, _)| external(app, fd)),
            _ => true,
        }),
        _ => true,
    }
}

impl Runtime {
    /// A reason to keep running behind: the agent; an audio stream, the
    /// microphone or Bluetooth open; a TCP upload in progress (bytes sent
    /// and not yet reported, or reported in the grace period; guard.rs); or a
    /// TCP connect not yet answered (bounded by its timeout), so the app hears
    /// how it ended and does not keep the slot frozen.
    fn own_facts(&self, i: usize, app: &App, now: u64) -> lifecycle::Facts {
        use device::Kind::*;
        // A wake lock keeps it running behind, but Cached (powerctl.rs `left_ns`).
        let left_ns = powerctl::left_ns(app.pid, app.used_ns, now);
        let sending = self.tx_stats.iter().flatten()
            .any(|t| t.index == i && t.pid == app.pid && (t.pending > 0 || now.wrapping_sub(t.last_ns) < lifecycle::GRACE_NS));
        let reason = app.path == appstore::INSTALLER || sending
            || app.fds.iter().flatten().any(|f| match f.borrow().kind {
                Audio { .. } | Mic { .. } | Hci { .. } => true,
                Tcp { slot: Some(s), .. } if self.net.state(s).0 == net::CONNECTING => true,
                // A connection offered or bytes sent to it lately (sockets.rs), so a server's clients do not hang.
                ref k => self.socket_wakes(k, now),
            });
        lifecycle::Facts { front: self.foreground == Some(i), home: self.home == Some(i), reason, shield: app.frozen.is_some(), left_ns }
    }

    /// Its own facts, or for a child process those it takes from the app
    /// that started it (lifecycle::inherit).
    fn life_facts(&self, i: usize, app: &App, now: u64) -> lifecycle::Facts {
        let own = self.own_facts(i, app, now);
        let r = self.root(i);
        match self.apps[r].as_ref() {
            Some(top) if r != i => lifecycle::inherit(own, &self.own_facts(r, top, now)),
            _ => own,
        }
    }

    /// The oldest ancestor of app `i` still running (lifecycle::root_of).
    fn root(&self, i: usize) -> usize {
        let index_of = |pid: u32| (0..MAX_APPS).find(|&p| pid != 0 && self.apps[p].as_ref().is_some_and(|q| q.pid == pid));
        let parent: Vec<Option<usize>> = (0..MAX_APPS).map(|n| self.apps[n].as_ref().and_then(|a| index_of(a.sig.ppid))).collect();
        lifecycle::root_of(&parent, i)
    }

    /// Every app as the killer sees it. Never closed: the app in front, Home,
    /// the agent, a Shield-held app, and the children of the app in front, of
    /// Home and of `caller`'s family.
    fn candidates(&self, caller: Option<usize>) -> Vec<Option<lifecycle::Candidate>> {
        let now = sys::now_ns();
        let family = caller.map(|c| self.root(c));
        (0..MAX_APPS)
            .map(|i| {
                let app = self.apps[i].as_ref()?;
                let f = self.life_facts(i, app, now);
                let r = self.root(i);
                let kept = self.foreground == Some(r) || self.home == Some(r) || Some(r) == family;
                Some(lifecycle::Candidate {
                    state: lifecycle::next_state(&f, now, self.life.pressure),
                    protected: f.front || f.home || f.shield || kept || app.path == appstore::INSTALLER,
                    core: app.path == wifi::SETTINGS_APP,
                    audio: app.fds.iter().flatten().any(|fd| matches!(fd.borrow().kind, device::Kind::Audio { .. })),
                    used_ns: app.used_ns,
                    heap: pipe_bytes(app),
                })
            })
            .collect()
    }

    /// The app to close for memory, of class up to `max_class` (lifecycle::class).
    fn victim(&self, caller: Option<usize>, max_class: u8) -> Option<usize> {
        lifecycle::pick_victim(&self.candidates(caller), max_class)
    }

    /// The app to close for a page fault by app `i` (lifecycle::fault_victim):
    /// any app behind for the app in front, Home, the agent or their
    /// children; only frozen and cached ones for anyone else.
    fn fault_victim_for(&self, i: usize) -> Option<usize> {
        let r = self.root(i);
        let privileged = self.foreground == Some(r) || self.home == Some(r) || self.apps[r].as_ref().is_some_and(|a| a.path == appstore::INSTALLER);
        lifecycle::fault_victim(&self.candidates(Some(i)), privileged)
    }

    /// Thread `slot` of an app the lifecycle froze ran into the runtime (a
    /// call or a fault that was queued as it froze): it is suspended with
    /// the others and resumed when the app thaws.
    pub fn hold_thread(&mut self, i: usize, slot: usize) {
        let Some(app) = self.apps[i].as_mut() else { return };
        sys::tcb_suspend(app.threads[slot].tcb);
        if !app.life.parked.contains(&slot) {
            app.life.parked.push(slot);
        }
    }

    /// A call by a thread of a lifecycle-frozen app is not made: the thread
    /// is answered back onto its svc instruction (`again`: r0..r7 as they
    /// were and the FaultIP of the call itself) and held, so it makes the
    /// call again when the app thaws.
    pub fn defer_call(&mut self, i: usize, slot: usize, again: &[u32; 9]) {
        sys::reply(again);
        self.hold_thread(i, slot);
    }

    /// Close app `i` for memory: status 137, logged and kept for `memory`.
    /// Opening it again starts it afresh.
    fn close_for_memory(&mut self, i: usize, reason: &'static str) {
        let Some(app) = self.apps[i].as_mut() else { return };
        app.exit_code = Some(lifecycle::EXIT_CLOSED);
        let pages = app.space.usage().0 + app.surface.as_ref().map_or(0, |s| s.pages.len());
        let (name, state) = (app.name.clone(), app.life.state.name());
        let place = if self.foreground == Some(i) { "in front" } else { "in the background" };
        self.reap(i);
        log!("low on app memory: closed {} {} ({} KiB free; it was {} with {} KiB: {})", name, place, self.pool.available() * sys::PAGE_KIB, state, pages * sys::PAGE_KIB, reason);
        self.life.note(lifecycle::Closed { name, reason, kib: pages * sys::PAGE_KIB, at_s: sys::now_ns() / 1_000_000_000 });
        self.shell.dirty = true;
    }

    /// Freeze app `i`: suspend its running threads; the rest are blocked in
    /// a call, whose wait is not served until it thaws.
    fn park(&mut self, i: usize) {
        let Some(app) = self.apps[i].as_mut() else { return };
        let parked: Vec<usize> = (0..app.threads.len()).filter(|&s| app.threads[s].alive && app.threads[s].reply.is_none()).collect();
        for &s in &parked {
            sys::tcb_suspend(app.threads[s].tcb);
        }
        app.life = lifecycle::Life { state: lifecycle::State::Frozen, parked };
        log!("lifecycle: froze {} (pid {}) in the background", app.name, app.pid);
    }

    /// Thaw app `i` into `state`. A Shield freeze that began meanwhile owns
    /// the threads; they stay as the Shield left them.
    fn unpark(&mut self, i: usize, state: lifecycle::State) {
        let Some(app) = self.apps[i].as_mut() else { return };
        let parked = core::mem::take(&mut app.life.parked);
        if app.frozen.is_none() {
            for s in parked {
                if app.threads[s].alive {
                    sys::tcb_resume(app.threads[s].tcb);
                }
            }
        }
        app.life.state = state;
    }

    /// Every pass: the app in front runs at once. Every second: each app's
    /// state, memory pressure, and closing an app when the pool or the heap
    /// is nearly gone.
    fn lifecycle_tick(&mut self, now: u64) {
        use lifecycle::{Pressure, State};
        if let Some(f) = self.foreground {
            match self.apps[f].as_ref().map(|a| a.life.state) {
                Some(State::Frozen) => self.unpark(f, State::Foreground),
                Some(s) if s != State::Foreground => self.apps[f].as_mut().unwrap().life.state = State::Foreground,
                _ => {}
            }
        }
        if now.wrapping_sub(self.life.last_ns) < 1_000_000_000 {
            return;
        }
        self.life.last_ns = now;
        let p = self.update_pressure(now);
        for i in 0..MAX_APPS {
            let Some(app) = self.apps[i].as_ref() else { continue };
            let (cur, next) = (app.life.state, lifecycle::next_state(&self.life_facts(i, app, now), now, p));
            if cur == next {
                continue;
            }
            if next == State::Frozen {
                self.park(i);
            } else if cur == State::Frozen {
                self.unpark(i, next);
            } else {
                self.apps[i].as_mut().unwrap().life.state = next;
            }
        }
        // The pool: a frozen or cached app. The heap: whoever holds the most
        // of it (its pipes) first, in any state but protected (lifecycle::heap_victim).
        if p == Pressure::Critical {
            if let Some(v) = self.victim(None, 1) {
                self.close_for_memory(v, "memory pressure");
            }
        } else if heap::HEAP.stats().1 < heap::LOW_BYTES {
            if let Some(v) = lifecycle::heap_victim(&self.candidates(None)) {
                self.close_for_memory(v, "runtime heap");
            }
        }
    }

    /// The pool's pressure now; on the way out of Normal, background apps
    /// hear LOW_MEMORY once. Also run before an on-demand close, so they hear
    /// it even when one fault drains the pool between two ticks.
    fn update_pressure(&mut self, now: u64) -> lifecycle::Pressure {
        let was = self.life.pressure;
        let p = lifecycle::pressure(self.pool.available(), RESERVE_PAGES, was);
        self.life.pressure = p;
        if was == lifecycle::Pressure::Normal && p != lifecycle::Pressure::Normal {
            log!("lifecycle: app memory low ({} KiB free): background apps asked to free memory", self.pool.available() * sys::PAGE_KIB);
            let behind: Vec<usize> = (0..MAX_APPS).filter(|&i| self.apps[i].is_some() && self.foreground != Some(i)).collect();
            for i in behind {
                self.event(i, abi::InputEvent { timestamp_us: now / 1000, kind: abi::EVENT_LIFECYCLE, value: abi::LIFECYCLE_LOW_MEMORY, ..Default::default() });
            }
        }
        p
    }

    /// `memory`: the pool, the heap, the app limit and each app's pages,
    /// screen surface, page tables and state, then pressure and closings (KiB).
    /// `only`: an app without the system capability sees its own entry and
    /// the pressure, not which other apps run, their states or closings.
    pub fn lifecycle_report(&self, only: Option<usize>) -> String {
        let (_, heap_free) = heap::HEAP.stats();
        let running = self.apps.iter().flatten().count();
        let mut out = alloc::format!(
            "free={} total={} apps_pool={} heap_free={} heap={} apps={}/{} refused={}",
            self.pool.available() * sys::PAGE_KIB, self.pool.total * sys::PAGE_KIB, self.pool.app_pages * sys::PAGE_KIB, heap_free / 1024,
            heap::HEAP.total() / 1024, running, self.app_limit, self.pool.refused
        );
        for (i, a) in self.apps.iter().enumerate() {
            let Some(a) = a.as_ref().filter(|_| only.map_or(true, |o| o == i)) else { continue };
            let (pages, tables) = a.space.usage();
            let surface = a.surface.as_ref().map_or(0, |s| s.pages.len());
            out += &alloc::format!(" {}={}+{}(t{},{})", lifecycle::clean(&a.name), pages * sys::PAGE_KIB, surface * sys::PAGE_KIB, tables, a.life.state.name());
        }
        match only {
            None => self.life.report(&mut out),
            Some(_) => out += [" pressure=normal", " pressure=low", " pressure=critical"][self.life.pressure as usize],
        }
        out
    }
}

#[derive(Default)]
pub struct Stats {
    since: u64,
    /// The busiest system calls: (number, count).
    calls: Vec<(u32, u32)>,
    /// The slowest step of a Runtime poll: flush, focus, touch.
    slowest_step: [u64; 3],
    loops: u64,
    faults: u64,
    slowest_poll: u64,
    slowest_pass: u64,
    pass_start: u64,
    /// Time spent asleep waiting for events (with the timer).
    slept_ns: u64,
}

impl Stats {
    pub fn count(&mut self, number: u32) {
        if let Some(c) = self.calls.iter_mut().find(|c| c.0 == number) {
            c.1 += 1;
        } else if self.calls.len() < 64 {
            self.calls.push((number, 1));
        }
    }
    fn step(&mut self, i: usize, ns: u64) {
        self.slowest_step[i] = self.slowest_step[i].max(ns);
    }
}

/// Enough heap to report a failed start before the pool exists (the pool
/// then adds HEAP_PAGES of its own; see pool.rs).
/// 7.9 KiB: with the other statics, the package (code and zeroed data) must
/// stay within its 0x3e000-byte window.
static mut BOOT_HEAP: [u64; 1008] = [0; 1008];

/// Recovery's view of the runtime (contain.rs `recover`).
impl contain::Host for Runtime {
    fn slots(&self) -> usize {
        self.untypeds.len().min(MAX_APPS)
    }
    fn present(&self, i: usize) -> bool {
        self.apps.get(i).map_or(false, Option::is_some)
    }
    fn close(&mut self, i: usize) {
        if let Some(app) = self.apps[i].as_mut() {
            log!("runtime fault {}: closed {} (pid {})", contain::STATE.recovered(), app.name, app.pid);
            app.exit_code = Some(134);
        }
        self.reap(i);
    }
    fn revoke(&mut self, i: usize) {
        sys::revoke(self.untypeds[i]);
    }
    fn home(&mut self) -> &mut Option<usize> {
        &mut self.home
    }
    fn front(&mut self) -> &mut Option<usize> {
        &mut self.foreground
    }
    fn home_tried(&self) -> bool {
        self.home_tried
    }
    fn start_home(&mut self) {
        Runtime::start_home(self, false);
    }
    fn show_home(&mut self) {
        Runtime::show_home(self);
    }
}

/// Stop this thread for good: what the supervisor did on every fault before
/// containment, done from inside (SLOT_AUTHORITY is the runtime's own TCB).
fn halt() -> ! {
    loop {
        sys::tcb_suspend(sys::SLOT_AUTHORITY);
        sys::yield_now();
    }
}

pub fn run() -> ! {
    // The first start, or the supervisor's restart after a fault in this
    // process, with memory as the fault left it (contain.rs).
    match contain::STATE.enter() {
        contain::Entry::Cold => {}
        // A fault during boot: nothing here can be trusted, so stay down
        // (suspended, not spinning: the fault may come at EVENT_PRIORITY).
        contain::Entry::Boot => halt(),
        contain::Entry::Warm { runtime, served } => {
            // Set by `settle` below: the runtime, which is never freed or moved.
            let rt = unsafe { &mut *(runtime as *mut Runtime) };
            rt.recover(served);
            rt.run()
        }
    }
    Log::init();
    unsafe { heap::HEAP.add(core::ptr::addr_of_mut!(BOOT_HEAP) as usize, 1008 * 8) };
    let mut rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            Log::write(alloc::format!("[open] start failed: seL4 error {}\n", e).as_bytes());
            // Suspended, as halt() (written out here: it costs less code).
            loop {
                sys::tcb_suspend(sys::SLOT_AUTHORITY);
            }
        }
    };
    let inventory = request(GET, INVENTORY, 0);
    rt.audio = inventory.op == OK && inventory.value & (1 << AUDIO_COMPONENT) != 0;
    rt.camera = inventory.op == OK && inventory.value & (1 << CAMERA_COMPONENT) != 0;
    log!(
        "ready: {} files in /apps image, {} MiB for apps",
        rt.fs.files(),
        rt.pool.available() / sys::MIB_PAGES
    );
    if sys::timer_selftest() {
        sys::set_own_priority(sys::EVENT_PRIORITY);
        log!("event-driven: the timer works; waiting for events at priority {}", sys::EVENT_PRIORITY);
    } else {
        log!("no timer: polling at priority {}", sys::PRIORITY);
    }
    log!(
        "scheduling: {}, {} {}",
        if sys::mcs() { "MCS budgets" } else { "static priorities" },
        sys::cores(),
        if sys::cores() > 1 { "cores (the app in front on core 1, others on 2-3)" } else { "core" }
    );
    // On the heap for good, so a restart after a fault finds it (contain.rs).
    let rt = alloc::boxed::Box::leak(alloc::boxed::Box::new(rt));
    contain::STATE.settle(rt as *mut Runtime as usize);
    rt.run()
}
