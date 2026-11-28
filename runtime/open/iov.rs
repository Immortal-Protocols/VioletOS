//! readv and writev (syscall.rs, system calls 145 and 146): the walk over
//! the app's buffers, apart from the runtime so the host tests it
//! (tests/iov_host.rs). One call takes at most IOV_MAX buffers (EINVAL past
//! it, as Linux) and moves at most `slice` bytes in all (device.rs
//! RW_SLICE): the count comes back short and the rest is the next call's,
//! served in turn with other apps' calls.
use super::syscall::{EAGAIN, EFAULT, EINVAL};

pub const IOV_MAX: u32 = 1024;

/// What the walk needs of the runtime.
pub trait Vectored {
    /// Buffer `k` of the app's iovec array: (base, len), the base a whole
    /// app address (runtime/open/vma.rs Va); None if the app
    /// cannot be read there.
    fn entry(&mut self, k: u32) -> Option<(u64, u32)>;
    /// Move up to `len` bytes at `base`: the count, or an error.
    fn io(&mut self, base: u64, len: u32) -> i32;
}

pub enum Outcome {
    Done(i32),
    /// Nothing moved yet and the first buffer got EAGAIN: a blocking
    /// descriptor waits with this buffer, a non-blocking one answers EAGAIN.
    Wait { base: u64, len: u32 },
}

pub fn run(v: &mut impl Vectored, count: u32, slice: u32) -> Outcome {
    if count > IOV_MAX {
        return Outcome::Done(EINVAL);
    }
    let mut total = 0u32;
    for k in 0..count {
        let room = slice.saturating_sub(total);
        if room == 0 {
            break;
        }
        let Some((base, len)) = v.entry(k) else { return Outcome::Done(EFAULT) };
        let len = len.min(room);
        if len == 0 {
            continue;
        }
        let r = v.io(base, len);
        if r == EAGAIN && total == 0 {
            return Outcome::Wait { base, len };
        }
        if r < 0 {
            return Outcome::Done(if total > 0 { total as i32 } else { r });
        }
        total += r as u32;
        if (r as u32) < len {
            break;
        }
    }
    Outcome::Done(total as i32)
}
