//! The Sentinel's request handler: bytes in, six reply words and output bytes
//! out. Everything the Open Runtime sends is treated as hostile: a frame must
//! match the CRC and sequence number announced with it and decode exactly
//! (common.rs `Request::decode`), every field must be what the operation
//! expects, and anything else is answered V_BAD, which the stub takes as a
//! refusal. Nothing here touches the kernel, so the host tests and the host
//! integration tests run this file as it ships (tests/, hostlink.rs).
//!
//! State: the user's consent answers (`Grants`), a generation number that
//! rises with every change to them, and the sealing key. The answers leave
//! only sealed; a sealed store is taken back only if this key sealed it and
//! it is not older than the one already held (within one start: across
//! restarts there is nowhere to keep the number, so after a restart an older
//! store this key sealed is accepted, see README.md). With no key (`unkeyed`:
//! no device root key where one is required) nothing is sealed, opened or
//! loaded: answers hold for this start only.
#![allow(dead_code)]
#[path = "common.rs"]
pub mod common;
#[path = "decide.rs"]
pub mod decide;
#[path = "seal.rs"]
pub mod seal;

use alloc::string::String;
use alloc::vec::Vec;
use common::*;
use decide::{Grants, Profile};

/// Operations this file does not know (the scorer's turn, scoring.rs).
pub trait Extension {
    /// The verdict and value, with any output in `out`; None for a bad request.
    fn handle(&mut self, req: &Request<'_>, out: &mut Vec<u8>) -> Option<(u32, u32)>;
}
pub struct NoExtension;
impl Extension for NoExtension {
    fn handle(&mut self, _: &Request<'_>, _: &mut Vec<u8>) -> Option<(u32, u32)> {
        None
    }
}

/// The label the consent store is sealed under. OP_SEAL and OP_UNSEAL refuse
/// it: the runtime can neither read the store nor have one made up.
const GRANTS_LABEL: &str = "grants";
/// Runtime state OP_SEAL protects: the Shield's saved mode.
pub const STATE_LABELS: [&str; 1] = ["shield-mode"];

pub struct Engine {
    sealer: Option<seal::Sealer>,
    grants: Grants,
    generation: u32,
}

fn verdict(d: Decision) -> u32 {
    match d {
        Decision::Allow => V_ALLOW,
        Decision::Deny => V_DENY,
        Decision::Ask => V_ASK,
    }
}

fn cap_value(cap: Option<Cap>) -> u32 {
    cap.map_or(0, |c| 1 + c as u32)
}

impl Engine {
    pub fn new(master: [u8; 32]) -> Engine {
        Engine { sealer: Some(seal::Sealer::new(master)), grants: Grants::default(), generation: 0 }
    }
    /// No sealing key: every seal, open and load is refused.
    pub fn unkeyed() -> Engine {
        Engine { sealer: None, grants: Grants::default(), generation: 0 }
    }

    /// Answer the request `frame`, announced with `seq` and `crc`.
    pub fn serve(&mut self, seq: u32, crc: u32, frame: &[u8], out: &mut Vec<u8>, ext: &mut dyn Extension) -> [u32; 6] {
        out.clear();
        let (verdict, value) = match self.handle(seq, crc, frame, out, ext) {
            Some(answer) if out.len() <= WINDOW => answer,
            _ => {
                out.clear();
                (V_BAD, 0)
            }
        };
        reply_words(seq, verdict, value, out)
    }

    fn handle(&mut self, seq: u32, crc: u32, frame: &[u8], out: &mut Vec<u8>, ext: &mut dyn Extension) -> Option<(u32, u32)> {
        if crc32(frame) != crc {
            return None;
        }
        let req = Request::decode(frame)?;
        if req.seq != seq {
            return None;
        }
        let fields = |n: usize| (req.count == n).then_some(());
        let flag = |v: u8| (v <= 1).then_some(v == 1);
        Some(match req.op {
            OP_ASK => {
                fields(2)?;
                let (origin, cap) = (Origin::from_wire(req.a)?, Cap::from_wire(req.b)?);
                let (owner, manifest) = (req.text(0, OWNER_MAX)?, req.text(1, MANIFEST_MAX)?);
                (verdict(decide::ask(&Profile::parse(manifest, origin), &self.grants, owner, cap)), 0)
            }
            OP_PATH => {
                fields(3)?;
                let (origin, write) = (Origin::from_wire(req.a)?, flag(req.b)?);
                let (owner, manifest, path) = (req.text(0, OWNER_MAX)?, req.text(1, MANIFEST_MAX)?, req.text(2, PATH_MAX)?);
                match decide::path_rule(owner, &Profile::parse(manifest, origin), path, write) {
                    Ok(()) => (V_ALLOW, 0),
                    Err(errno) => (V_DENY, errno as u32),
                }
            }
            OP_DEVICE => {
                fields(1)?;
                match decide::device_cap(req.text(0, PATH_MAX)?) {
                    Ok(cap) => (V_ALLOW, cap_value(cap)),
                    Err(()) => (V_DENY, 0),
                }
            }
            OP_SYS => {
                // The command's first word and, if it has one, its second.
                if req.count == 0 || req.count > 2 {
                    return None;
                }
                let mut line = String::from(req.text(0, 64)?);
                if req.count == 2 {
                    line.push(' ');
                    line.push_str(req.text(1, 64)?);
                }
                (V_ALLOW, cap_value(decide::sys_cap(&line)))
            }
            OP_EXEC => {
                fields(2)?;
                let core = flag(req.a)?;
                (V_ALLOW, decide::exec_crosses(req.text(0, OWNER_MAX)?, core, req.text(1, PATH_MAX)?) as u32)
            }
            OP_LOAD => {
                fields(1)?;
                match self.unsealed_grants(req.text(0, WINDOW)?) {
                    Some((generation, grants)) if generation >= self.generation => {
                        self.generation = generation;
                        self.grants = grants;
                        (V_ALLOW, self.grants.rows.len() as u32)
                    }
                    // Not sealed by this key, damaged, or older than what is
                    // held: the store stays as it is (empty after a start).
                    _ => (V_DENY, self.grants.rows.len() as u32),
                }
            }
            OP_RECORD => {
                fields(1)?;
                let (cap, allow) = (Cap::from_wire(req.a)?, flag(req.b)?);
                self.grants.record(req.text(0, OWNER_MAX)?, cap, allow);
                self.changed(out)
            }
            OP_FORGET => {
                fields(1)?;
                self.grants.forget(req.text(0, OWNER_MAX)?);
                self.changed(out)
            }
            OP_EXPORT => {
                fields(0)?;
                self.exported(out)
            }
            OP_SEAL => {
                fields(2)?;
                let label = req.text(0, 32).filter(|l| STATE_LABELS.contains(l))?;
                let value = req.fields[1];
                if value.len() > SEAL_MAX {
                    return None;
                }
                match &self.sealer {
                    Some(s) => {
                        out.extend_from_slice(s.seal(label, value).as_bytes());
                        (V_ALLOW, 0)
                    }
                    None => (V_DENY, 0),
                }
            }
            OP_UNSEAL => {
                fields(2)?;
                let label = req.text(0, 32).filter(|l| STATE_LABELS.contains(l))?;
                match self.sealer.as_ref().and_then(|s| s.open(label, req.text(1, WINDOW)?)) {
                    Some(value) => {
                        out.extend_from_slice(&value);
                        (V_ALLOW, 0)
                    }
                    None => (V_DENY, 0),
                }
            }
            _ => return ext.handle(&req, out),
        })
    }

    fn changed(&mut self, out: &mut Vec<u8>) -> (u32, u32) {
        self.generation = self.generation.saturating_add(1);
        self.exported(out)
    }

    /// The sealed store as output; DENY (and nothing to save) with no key.
    fn exported(&self, out: &mut Vec<u8>) -> (u32, u32) {
        match self.export() {
            Some(text) => {
                out.extend_from_slice(text.as_bytes());
                (V_ALLOW, self.grants.rows.len() as u32)
            }
            None => (V_DENY, self.grants.rows.len() as u32),
        }
    }

    /// The store, sealed: "g GENERATION" on a line, then the answers.
    fn export(&self) -> Option<String> {
        let sealer = self.sealer.as_ref()?;
        let mut plain = String::from("g ");
        let (mut digits, mut n, mut v) = ([0u8; 10], 0, self.generation);
        loop {
            digits[n] = b'0' + (v % 10) as u8;
            n += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        while n > 0 {
            n -= 1;
            plain.push(digits[n] as char);
        }
        plain.push('\n');
        plain.push_str(&self.grants.format());
        Some(sealer.seal(GRANTS_LABEL, plain.as_bytes()))
    }

    fn unsealed_grants(&self, text: &str) -> Option<(u32, Grants)> {
        let plain = self.sealer.as_ref()?.open(GRANTS_LABEL, text)?;
        let plain = core::str::from_utf8(&plain).ok()?;
        let (head, rows) = plain.split_once('\n')?;
        Some((head.strip_prefix("g ")?.parse().ok()?, Grants::parse(rows)))
    }

    /// For the tests: the answers held now.
    pub fn grants(&self) -> &Grants {
        &self.grants
    }
}
