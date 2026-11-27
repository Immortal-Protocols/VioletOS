//! The disk the host tests run on, and the power-cut sweep.
//!
//! The disk keeps a log of every write and flush. From the log, `sweep`
//! rebuilds what the medium could hold had power been lost at each point:
//! the device may hold writes in a volatile cache until a flush returns, in
//! any order, so after a cut any subset of the unflushed sector writes may be
//! there, and the sector being written may be torn (its start new, its end
//! old). Each such image is handed to the caller, who mounts it and checks.
//! This is a model: what a real eMMC does on power loss is only established
//! on the handset.
use crate::blockdev::BlockDev;
use std::cell::RefCell;
use std::rc::Rc;

pub enum Op {
    Write { lba: u64, data: Vec<u8> },
    Flush,
}

#[derive(Default)]
pub struct State {
    /// What the running system reads: every write applied.
    pub image: Vec<u8>,
    pub log: Vec<Op>,
    pub recording: bool,
    /// The image when recording began (taken as durable).
    pub base: Vec<u8>,
    /// Writes and flushes so far, and the count at which they start failing.
    pub ops: usize,
    pub fail_at: Option<usize>,
    pub discarded: u64,
}

#[derive(Clone)]
pub struct TestDisk(pub Rc<RefCell<State>>);

impl TestDisk {
    pub fn new(sectors: usize) -> Self {
        Self::from_image(vec![0xa5; sectors * 512])
    }
    pub fn from_image(image: Vec<u8>) -> Self {
        TestDisk(Rc::new(RefCell::new(State { image, ..State::default() })))
    }
    /// Log from here on. Call when everything written so far is flushed.
    pub fn record(&self) {
        let mut s = self.0.borrow_mut();
        s.base = s.image.clone();
        s.log.clear();
        s.recording = true;
    }
    pub fn log_len(&self) -> usize {
        self.0.borrow().log.len()
    }
    pub fn fail_at(&self, op: Option<usize>) {
        let mut s = self.0.borrow_mut();
        s.ops = 0;
        s.fail_at = op;
    }
    pub fn image(&self) -> Vec<u8> {
        self.0.borrow().image.clone()
    }
    fn step(s: &mut State) -> Result<(), ()> {
        if s.fail_at.is_some_and(|at| s.ops >= at) {
            return Err(());
        }
        s.ops += 1;
        Ok(())
    }
}

impl BlockDev for TestDisk {
    fn sectors(&self) -> u64 {
        (self.0.borrow().image.len() / 512) as u64
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        let s = self.0.borrow();
        let at = lba as usize * 512;
        if buf.len() % 512 != 0 || at + buf.len() > s.image.len() || s.fail_at.is_some_and(|f| s.ops >= f) {
            return Err(());
        }
        buf.copy_from_slice(&s.image[at..at + buf.len()]);
        Ok(())
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        let mut s = self.0.borrow_mut();
        let at = lba as usize * 512;
        if buf.len() % 512 != 0 || at + buf.len() > s.image.len() {
            return Err(());
        }
        Self::step(&mut s)?;
        s.image[at..at + buf.len()].copy_from_slice(buf);
        if s.recording {
            s.log.push(Op::Write { lba, data: buf.to_vec() });
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), ()> {
        let mut s = self.0.borrow_mut();
        Self::step(&mut s)?;
        if s.recording {
            s.log.push(Op::Flush);
        }
        Ok(())
    }
    fn discard(&mut self, _lba: u64, sectors: u64) -> Result<(), ()> {
        self.0.borrow_mut().discarded += sectors;
        Ok(())
    }
}

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Seeds per cut point: 2 by default, more with STORAGE_CRASH_SEEDS.
pub fn seeds() -> u64 {
    std::env::var("STORAGE_CRASH_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(2)
}

fn put(image: &mut [u8], lba: u64, sector: &[u8]) {
    let at = lba as usize * 512;
    image[at..at + 512].copy_from_slice(sector);
}
/// A torn sector: the first `split` bytes new, the rest as they were.
fn tear(image: &mut [u8], lba: u64, sector: &[u8], split: usize) {
    let at = lba as usize * 512;
    image[at..at + split].copy_from_slice(&sector[..split]);
}

fn apply(image: &mut [u8], op: &(u64, &[u8])) {
    let at = op.0 as usize * 512;
    image[at..at + op.1.len()].copy_from_slice(op.1);
}

/// For every point `cut` in the log (the first `cut` operations were issued
/// and operation `cut` was in flight), build the images a power cut there
/// could leave and pass each to `check(cut, image)`. Returns how many.
///   - nothing since the last flush reached the medium
///   - everything issued did, and the write in flight not at all
///   - everything issued did, and the write in flight partly: some of its
///     sectors, in order or not, one of them torn
///   - a random subset of all unflushed sectors did, one of them torn
///   - when the operation in flight is a flush: all unflushed writes but
///     one, and only one of them, for each of them in turn
pub fn sweep(base: &[u8], log: &[Op], mut check: impl FnMut(usize, Vec<u8>)) -> usize {
    let mut durable = base.to_vec();
    // Unflushed writes, in the order they were issued.
    let mut pending: Vec<(u64, &[u8])> = Vec::new();
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let mut images = 0;
    for cut in 0..=log.len() {
        let mut emit = |image: Vec<u8>| {
            images += 1;
            check(cut, image);
        };
        emit(durable.clone());
        let mut all = durable.clone();
        for op in &pending {
            apply(&mut all, op);
        }
        if !pending.is_empty() {
            emit(all.clone());
        }
        if let Some(Op::Write { lba, data }) = log.get(cut) {
            let n = data.len() / 512;
            for seed in 0..seeds() {
                // A prefix of the write, then a torn sector.
                let mut image = all.clone();
                let whole = rng.below(n as u64 + 1) as usize;
                for k in 0..whole {
                    put(&mut image, lba + k as u64, &data[k * 512..][..512]);
                }
                if whole < n && seed % 2 == 0 {
                    tear(&mut image, lba + whole as u64, &data[whole * 512..][..512], rng.below(512) as usize);
                }
                emit(image);
                // Any of its sectors, one torn.
                let mut image = all.clone();
                let torn = rng.below(n as u64) as usize;
                for k in 0..n {
                    let sector = &data[k * 512..][..512];
                    if k == torn {
                        tear(&mut image, lba + k as u64, sector, rng.below(512) as usize);
                    } else if rng.below(2) == 0 {
                        put(&mut image, lba + k as u64, sector);
                    }
                }
                emit(image);
            }
        }
        if pending.len() > 1 {
            for seed in 0..seeds() {
                let mut image = durable.clone();
                let keep = 1 + rng.below(3); // of 4
                let sectors: usize = pending.iter().map(|op| op.1.len() / 512).sum();
                let torn = rng.below(sectors as u64) as usize;
                let mut at = 0;
                for (lba, data) in &pending {
                    for k in 0..data.len() / 512 {
                        let sector = &data[k * 512..][..512];
                        if at == torn && seed % 2 == 0 {
                            tear(&mut image, lba + k as u64, sector, rng.below(512) as usize);
                        } else if rng.below(4) < keep {
                            put(&mut image, lba + k as u64, sector);
                        }
                        at += 1;
                    }
                }
                emit(image);
            }
            if matches!(log.get(cut), Some(Op::Flush)) {
                for skip in 0..pending.len() {
                    let mut image = durable.clone();
                    for (k, op) in pending.iter().enumerate() {
                        if k != skip {
                            apply(&mut image, op);
                        }
                    }
                    emit(image);
                    let mut image = durable.clone();
                    apply(&mut image, &pending[skip]);
                    emit(image);
                }
            }
        }
        match log.get(cut) {
            Some(Op::Write { lba, data }) => pending.push((*lba, data)),
            Some(Op::Flush) => {
                for op in pending.drain(..) {
                    apply(&mut durable, &op);
                }
            }
            None => {}
        }
    }
    images
}
