//! Host tests for readv and writev's walk over an app's buffers
//! (runtime/open/iov.rs, `make test-unit`): one call takes at most IOV_MAX
//! buffers and moves at most one slice in all, however many it is given.
//!     rustc --edition=2021 --test -O runtime/open/tests/iov_host.rs -o build/tests/scale/iov
#[path = "../iov.rs"]
mod iov;

mod syscall {
    pub const EAGAIN: i32 = -11;
    pub const EFAULT: i32 = -14;
    pub const EINVAL: i32 = -22;
}

use iov::{run, Outcome, Vectored, IOV_MAX};

const SLICE: u32 = 1 << 20;

/// An app's iovec array, and a descriptor that takes `room` bytes a call
/// (u32::MAX: everything, as /dev/null or a file) or answers `error`.
struct App {
    iov: Vec<(u32, u32)>,
    room: u32,
    error: Option<i32>,
    moved: u64,
    calls: u32,
}
impl Vectored for App {
    fn entry(&mut self, k: u32) -> Option<(u32, u32)> {
        self.iov.get(k as usize).copied()
    }
    fn io(&mut self, _base: u32, len: u32) -> i32 {
        self.calls += 1;
        if let Some(e) = self.error {
            return e;
        }
        let n = len.min(self.room);
        self.room -= n;
        self.moved += n as u64;
        n as i32
    }
}
fn app(iov: Vec<(u32, u32)>) -> App {
    App { iov, room: u32::MAX, error: None, moved: 0, calls: 0 }
}
fn done(o: Outcome) -> i32 {
    match o {
        Outcome::Done(r) => r,
        Outcome::Wait { .. } => panic!("waits"),
    }
}

#[test]
fn a_million_full_buffers_move_one_slice_in_one_call() {
    // The attack: every iovec names the same 1 MiB buffer, to /dev/null.
    let mut a = app(vec![(0x1000_0000, SLICE); 1_000_000]);
    let n = a.iov.len() as u32;
    assert_eq!(done(run(&mut a, n, SLICE)), syscall::EINVAL, "past IOV_MAX");
    assert_eq!(a.calls, 0);
    let mut a = app(vec![(0x1000_0000, SLICE); IOV_MAX as usize]);
    assert_eq!(done(run(&mut a, IOV_MAX, SLICE)), SLICE as i32);
    assert_eq!((a.moved, a.calls), (SLICE as u64, 1), "one slice, then the call returns short");
}

#[test]
fn the_slice_is_the_total_across_buffers_and_never_overflows() {
    let mut a = app(vec![(0x1000_0000, 3 << 18); 64]);
    assert_eq!(done(run(&mut a, 64, SLICE)), SLICE as i32);
    assert_eq!(a.moved, SLICE as u64);
    // Huge lengths: clamped to what is left, the count fits an i32.
    let mut a = app(vec![(0x1000_0000, u32::MAX); 1024]);
    assert_eq!(done(run(&mut a, 1024, SLICE)), SLICE as i32);
    // Small buffers all move.
    let mut a = app(vec![(0x1000_0000, 100); 10]);
    assert_eq!(done(run(&mut a, 10, SLICE)), 1000);
}

#[test]
fn zero_length_buffers_are_bounded_by_iov_max() {
    let mut a = app(vec![(0, 0); IOV_MAX as usize + 1]);
    assert_eq!(done(run(&mut a, IOV_MAX, SLICE)), 0);
    assert_eq!(a.calls, 0);
    assert_eq!(done(run(&mut a, IOV_MAX + 1, SLICE)), syscall::EINVAL);
}

#[test]
fn short_counts_errors_and_waits_keep_their_meaning() {
    // A short count ends the call with what moved.
    let mut a = app(vec![(0x1000, 100), (0x2000, 100), (0x3000, 100)]);
    a.room = 150;
    assert_eq!(done(run(&mut a, 3, SLICE)), 150);
    assert_eq!(a.calls, 2);
    // An unreadable iovec array: EFAULT.
    let mut a = app(vec![(0x1000, 100)]);
    assert_eq!(done(run(&mut a, 2, SLICE)), syscall::EFAULT);
    // EAGAIN before anything moved: the caller may wait on that buffer.
    let mut a = app(vec![(0, 0), (0x2000, 5000)]);
    a.error = Some(syscall::EAGAIN);
    assert!(matches!(run(&mut a, 2, SLICE), Outcome::Wait { base: 0x2000, len: 5000 }));
    // An error with nothing moved is the answer.
    let mut a = app(vec![(0x1000, 100), (0x2000, 100)]);
    a.error = Some(-5);
    assert_eq!(done(run(&mut a, 2, SLICE)), -5);
    assert_eq!(a.calls, 1);
}
