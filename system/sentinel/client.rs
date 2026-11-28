//! The asking side of the Sentinel's protocol (common.rs), as the Open
//! Runtime's enforcement stub uses it (runtime/open/policy.rs). It decides
//! nothing: it frames a question, checks that the reply is a reply to that
//! question, and hands back the Sentinel's answer or None. None (the Sentinel
//! cannot be reached, the reply's magic or sequence number is wrong, its
//! output does not match its CRC or is longer than the window) is for the
//! caller to treat as a refusal: every function in the stub does.
#![allow(dead_code)]
use super::common::*;

/// The kernel edge: the two windows and the IPC call. The Open Runtime's is
/// in runtime/open/policy.rs; the host's joins this file to the real engine
/// (hostlink.rs).
pub trait Port {
    /// The request window.
    fn window(&mut self) -> &mut [u8];
    /// Announce the request of `len` bytes; the reply words, or None if the
    /// Sentinel cannot be reached.
    fn call(&mut self, seq: u32, len: u32, crc: u32) -> Option<[u32; 6]>;
    /// The reply window.
    fn output(&self) -> &[u8];
}

pub struct Answer {
    pub verdict: u32,
    pub value: u32,
    /// Bytes of output in the reply window.
    pub len: usize,
}

/// One question and its checked answer.
pub fn transact<P: Port>(port: &mut P, seq: u32, op: u8, a: u8, b: u8, fields: &[&[u8]]) -> Option<Answer> {
    let len = encode(port.window(), seq, op, a, b, fields)?;
    let crc = crc32(&port.window()[..len]);
    let r = port.call(seq, len as u32, crc)?;
    let out = port.output();
    let n = r[4] as usize;
    if r[0] != REPLY_MAGIC || r[1] != seq || n > out.len().min(WINDOW) || crc32(&out[..n]) != r[5] {
        return None;
    }
    // Only the four verdicts exist; V_BAD is the Sentinel's own refusal.
    matches!(r[2], V_ALLOW | V_DENY | V_ASK).then_some(Answer { verdict: r[2], value: r[3], len: n })
}

/// A capability in a reply's value: 0 for none, else its number plus one.
pub fn cap_of(value: u32) -> Result<Option<Cap>, ()> {
    match value {
        0 => Ok(None),
        v => u8::try_from(v - 1).ok().and_then(Cap::from_wire).map(Some).ok_or(()),
    }
}
