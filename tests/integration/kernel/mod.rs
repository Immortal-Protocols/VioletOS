//! The fakes: everything below the shipped code that a host cannot have.
//!
//! Only true hardware and kernel edges live here, each as small as it can be:
//!
//!   clock      the timer (`sys::now_ns`, `sys::pause`, `ticks`)
//!   ipc        seL4 endpoints (`ipc_call`, the symbol runtime/protocol.rs
//!              declares) and notification badges
//!   sys, pool  the frame and page-table calls and the page pool that
//!              system/supervisor/procmgr.rs maps app memory with
//!   Disk       a block device that can lose power (Storage's own test disk)
//!   Screen     the 720 x 1600 framebuffer
//!   Touch      the touch controller's report queue
//!   Link       the network link: frames handed to whoever is on the wire
//!
//! All of it is per thread (`thread_local!`), because the shipped code
//! reaches these edges through free functions. The test runner gives every
//! test its own thread, so every test has its own machine and they run in
//! parallel. Nothing here is a file, a port or a process-wide static.
use crate::protocol::Message;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

// ---- clock ----

thread_local! {
    static NOW_NS: Cell<u64> = const { Cell::new(0) };
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub mod clock {
    use super::NOW_NS;
    /// Nanoseconds since this machine started.
    pub fn now_ns() -> u64 {
        NOW_NS.with(|n| n.get())
    }
    pub fn advance_ns(ns: u64) {
        NOW_NS.with(|n| n.set(n.get() + ns));
    }
    pub fn set_ns(ns: u64) {
        NOW_NS.with(|n| n.set(ns));
    }
}

/// The log page: every `log!` line of the shipped code, in order.
pub mod serial {
    use super::LOG;
    pub fn write(line: String) {
        LOG.with(|l| l.borrow_mut().push(line));
    }
    pub fn lines() -> Vec<String> {
        LOG.with(|l| l.borrow().clone())
    }
    pub fn text() -> String {
        lines().join("\n")
    }
    pub fn clear() {
        LOG.with(|l| l.borrow_mut().clear());
    }
}

// ---- seL4 IPC: endpoints and notifications ----

/// What answers on an endpoint: a service's own request handler.
pub type Handler = Box<dyn FnMut(Message) -> Message>;

thread_local! {
    static ENDPOINTS: RefCell<BTreeMap<u32, Rc<RefCell<Handler>>>> = RefCell::new(BTreeMap::new());
    static CALLS: RefCell<Vec<(u32, Message)>> = const { RefCell::new(Vec::new()) };
    static BADGES: Cell<u32> = const { Cell::new(0) };
}

pub mod ipc {
    use super::*;
    /// Put a service behind capability slot `slot` of the calling process.
    pub fn serve(slot: u32, handler: impl FnMut(Message) -> Message + 'static) {
        ENDPOINTS.with(|e| e.borrow_mut().insert(slot, Rc::new(RefCell::new(Box::new(handler) as Handler))));
    }
    pub fn served(slot: u32) -> bool {
        ENDPOINTS.with(|e| e.borrow().contains_key(&slot))
    }
    pub fn close(slot: u32) {
        ENDPOINTS.with(|e| e.borrow_mut().remove(&slot));
    }
    /// Every call made so far: (slot, request).
    pub fn calls() -> Vec<(u32, Message)> {
        CALLS.with(|c| c.borrow().clone())
    }
    /// seL4_Call. An endpoint nobody serves answers OFFLINE, like a
    /// service that is not installed.
    pub fn call(slot: u32, m: Message) -> Message {
        CALLS.with(|c| c.borrow_mut().push((slot, m)));
        let handler = ENDPOINTS.with(|e| e.borrow().get(&slot).cloned());
        match handler {
            Some(h) => (h.borrow_mut())(m),
            None => Message::error(crate::protocol::OFFLINE),
        }
    }
    /// seL4_Signal on a notification: the badge bits are OR'd until taken.
    pub fn signal(badge: u32) {
        BADGES.with(|b| b.set(b.get() | badge));
    }
    /// seL4_Wait (non-blocking): the pending badges, cleared.
    pub fn take() -> u32 {
        BADGES.with(|b| b.replace(0))
    }
}

// The symbols runtime/protocol.rs declares (runtime/transport.c on the phone).
#[no_mangle]
extern "C" fn ipc_call(slot: u32, m: &mut Message) {
    *m = ipc::call(slot, *m);
}
#[no_mangle]
extern "C" fn ipc_receive(_slot: u32, _m: &mut Message) -> u32 {
    0
}
#[no_mangle]
extern "C" fn ipc_reply(_m: &mut Message) {}
#[no_mangle]
extern "C" fn ticks() -> u32 {
    (clock::now_ns() / 1_000_000) as u32
}
#[no_mangle]
static ticks_per_ms: u32 = 1;
#[no_mangle]
extern "C" fn delay_ms(ms: u32) {
    clock::advance_ns(u64::from(ms) * 1_000_000);
}
#[no_mangle]
extern "C" fn hold_cpu(_on: u32) {}

// ---- frames and page tables ----

/// The calls of runtime/open/sys.rs that the shipped host-buildable code
/// makes, recording what the app's hardware mapping of every page would be.
/// `page_map` panics on a mapping that is writable and executable, so every
/// test that maps memory also checks W^X.
pub mod sys {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    /// runtime/open/sys.rs ships 4 KiB pages; run.sh also builds the
    /// suite with `--cfg page64` for the 64 KiB configuration procmgr.rs
    /// still supports (as runtime/open/tests/paging_host.rs does).
    pub const PAGE_BITS: u32 = if cfg!(page64) { 16 } else { 12 };
    pub const PAGE: usize = 1 << PAGE_BITS;
    pub const SLOT_RUNTIME: u32 = 2;
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Object {
        PageDirectory,
        PageTable,
        CNode,
    }
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Mapping {
        pub pd: u32,
        pub va: u64,
        pub writable: bool,
        pub exec: bool,
    }
    #[derive(Default)]
    pub struct Hardware {
        /// Frame capability -> how it is mapped in its app now.
        pub mapped: BTreeMap<u32, Mapping>,
        pub maps: u32,
        /// Frame capability -> the pool page it is a copy of.
        pub frames: BTreeMap<u32, u32>,
        /// Cache flushes asked for (code pages made executable).
        pub unified: Vec<u32>,
    }
    thread_local!(pub static HW: RefCell<Hardware> = RefCell::new(Hardware::default()));

    pub fn now_ns() -> u64 {
        super::clock::now_ns()
    }
    /// A millisecond passes: polling loops in the shipped code make progress.
    pub fn pause() {
        super::clock::advance_ns(1_000_000);
    }
    pub fn yield_now() {
        super::clock::advance_ns(1_000);
    }
    pub fn counter() -> u64 {
        super::clock::now_ns()
    }
    pub fn asid_assign(_pd: u32) -> Result<(), i32> {
        Ok(())
    }
    pub fn pt_map(_pt: u32, _pd: u32, _va: u64) -> Result<(), i32> {
        Ok(())
    }
    pub fn copy(dest: u32, src: u32) -> Result<(), i32> {
        HW.with(|h| h.borrow_mut().frames.insert(dest, src));
        Ok(())
    }
    pub fn page_map(frame: u32, pd: u32, va: u64, writable: bool, exec: bool) -> Result<(), i32> {
        assert!(!(writable && exec), "a page mapped writable and executable at {va:#x}");
        assert_eq!(va as usize & (PAGE - 1), 0, "a mapping not on a page boundary");
        HW.with(|h| {
            let mut h = h.borrow_mut();
            if let Some(old) = h.mapped.get(&frame) {
                // seL4 changes rights in place only at the same address.
                assert_eq!(old.va, va, "a frame mapped at two addresses");
            }
            h.maps += 1;
            h.mapped.insert(frame, Mapping { pd, va, writable, exec });
            Ok(())
        })
    }
    pub fn page_unmap(frame: u32) {
        HW.with(|h| h.borrow_mut().mapped.remove(&frame));
    }
    pub fn page_unify(frame: u32) {
        HW.with(|h| {
            let mut h = h.borrow_mut();
            assert!(h.mapped.contains_key(&frame), "seL4 refuses to flush a frame that is not mapped");
            h.unified.push(frame);
        });
    }
    pub fn delete(slot: u32) {
        page_unmap(slot);
    }
    /// The hardware mapping of the page at `va` in address space
    /// `pd`: (writable, executable), or None if nothing is mapped there.
    pub fn at(pd: u32, va: u64) -> Option<(bool, bool)> {
        HW.with(|h| h.borrow().mapped.values().find(|m| m.pd == pd && m.va == va & !(PAGE as u64 - 1)).map(|m| (m.writable, m.exec)))
    }
    /// What the MMU would do with `access` at `va`: Ok, or the fault.
    pub fn mmu(pd: u32, va: u64, write: bool, exec: bool) -> Result<(), &'static str> {
        match at(pd, va) {
            None => Err("translation fault"),
            Some((w, _)) if write && !w => Err("permission fault (write)"),
            Some((_, x)) if exec && !x => Err("permission fault (execute never)"),
            Some(_) => Ok(()),
        }
    }
}

/// The page pool procmgr.rs takes app memory from (runtime/open/pool.rs on
/// the phone, where it is a window of seL4 frames at a fixed address).
pub mod pool {
    use super::sys::{Object, PAGE};
    use std::cell::RefCell;
    /// 16 MiB per machine unless the test asks for another size.
    pub const PAGES: usize = (16 << 20) / PAGE;
    thread_local!(static MEMORY: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) });
    pub fn page_va(index: u32) -> usize {
        MEMORY.with(|m| {
            let mut m = m.borrow_mut();
            if m.is_empty() {
                m.resize(PAGES * PAGE, 0);
            }
            m.as_mut_ptr() as usize + index as usize * PAGE
        })
    }
    pub struct Kernel {
        next: u32,
    }
    impl Kernel {
        pub fn new() -> Kernel {
            Kernel { next: 1000 }
        }
        pub fn slot(&mut self) -> u32 {
            self.next += 1;
            self.next
        }
        pub fn release(&mut self, _slot: u32) {}
        pub fn object(&mut self, _untyped: u32, _o: Object, _bits: u32) -> Result<u32, i32> {
            Ok(self.slot())
        }
    }
    pub struct Pool {
        free: Vec<u32>,
        pub total: usize,
    }
    impl Pool {
        pub fn new() -> Pool {
            Pool::with_pages(PAGES)
        }
        /// A pool of `pages` pages (at most PAGES).
        pub fn with_pages(pages: usize) -> Pool {
            assert!(pages <= PAGES);
            Pool { free: (0..pages as u32).rev().collect(), total: pages }
        }
        pub fn cap(&self, index: u32) -> u32 {
            index
        }
        pub fn available(&self) -> usize {
            self.free.len()
        }
        pub fn take(&mut self) -> Option<u32> {
            let index = self.free.pop()?;
            unsafe { core::ptr::write_bytes(page_va(index) as *mut u8, 0, PAGE) };
            Some(index)
        }
        pub fn give_back(&mut self, index: u32) {
            self.free.push(index);
        }
    }
}

// ---- block device ----

/// The disk: Storage's own test disk (services/storage/tests/testdisk.rs),
/// which logs writes and flushes and can rebuild what a power cut leaves.
pub use crate::testdisk::TestDisk as Disk;

// ---- framebuffer ----

pub const WIDTH: usize = 720;
pub const HEIGHT: usize = 1600;

/// The panel's framebuffer: what the viewer of the emulator would show.
pub struct Screen {
    pub pixels: Vec<u32>,
}
impl Default for Screen {
    fn default() -> Self {
        Screen { pixels: vec![0; WIDTH * HEIGHT] }
    }
}
impl Screen {
    pub fn pixel(&self, x: usize, y: usize) -> u32 {
        self.pixels[y * WIDTH + x]
    }
    pub fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, color: u32) {
        for row in y..(y + h).min(HEIGHT) {
            for p in &mut self.pixels[row * WIDTH + x..row * WIDTH + (x + w).min(WIDTH)] {
                *p = color;
            }
        }
    }
    /// Rows [y0, y1) that hold `color` anywhere.
    pub fn any(&self, y0: usize, y1: usize, color: u32) -> bool {
        self.pixels[y0 * WIDTH..y1 * WIDTH].contains(&color)
    }
}

// ---- touch controller ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub x: i32,
    pub y: i32,
    pub down: bool,
}
/// The touch controller: reports queue until the system reads them.
#[derive(Default)]
pub struct Touch {
    pub reports: VecDeque<Report>,
}
impl Touch {
    pub fn press(&mut self, x: i32, y: i32) {
        self.reports.push_back(Report { x, y, down: true });
    }
    pub fn release(&mut self, x: i32, y: i32) {
        self.reports.push_back(Report { x, y, down: false });
    }
}

// ---- network link ----

/// The network link: datagrams the phone sends, and a scripted peer that may
/// answer each one (an NTP server, a resolver, a host that never answers).
#[derive(Default)]
pub struct Link {
    pub sent: Vec<Vec<u8>>,
    pub peer: Option<Box<dyn FnMut(&[u8]) -> Option<Vec<u8>>>>,
    pub received: VecDeque<Vec<u8>>,
    /// Unplugged: frames go nowhere.
    pub down: bool,
}
impl Link {
    pub fn send(&mut self, frame: &[u8]) {
        self.sent.push(frame.to_vec());
        if self.down {
            return;
        }
        if let Some(peer) = self.peer.as_mut() {
            if let Some(answer) = peer(frame) {
                self.received.push_back(answer);
            }
        }
    }
    pub fn receive(&mut self) -> Option<Vec<u8>> {
        self.received.pop_front()
    }
}

/// A new machine on this thread: time zero, no log, no endpoints, no memory mapped.
pub fn reset() {
    clock::set_ns(0);
    serial::clear();
    ENDPOINTS.with(|e| e.borrow_mut().clear());
    CALLS.with(|c| c.borrow_mut().clear());
    BADGES.with(|b| b.set(0));
    sys::HW.with(|h| *h.borrow_mut() = sys::Hardware::default());
}
