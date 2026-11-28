//! The on-device scorer's turn, in the Sentinel. The Open Runtime sends the
//! telemetry it recorded since the last turn and who is running; the scorer
//! (runtime/open/scorer.rs, built into this component, not into the Open
//! Runtime) keeps each process's window here, scores the ones that changed
//! and decides what each deserves: nothing, a warning, quarantine, a freeze.
//! The reply is that decision and the record of it; the runtime carries it
//! out (guard.rs `score_tick`). Whether a program may be contained at all in
//! the present mode is decided here too (`shield::eligible_in`).
//!
//! Every byte of the request is the runtime's and is checked: lengths must be
//! whole records, the mode a mode, a subject's name length within its name.
#![allow(dead_code)]
use super::engine::common::*;
use super::engine::Extension;
use super::scorer::{Enforcer, Scorer};
use super::scorer_model as model;
use super::shield::{self, Mode, Subject, TelemetryEvent};
use alloc::vec::Vec;

// The runtime carries results out by these numbers (guard.rs).
const _: () = assert!(super::scorer::QUARANTINE == SCORE_QUARANTINE && super::scorer::FREEZE == SCORE_FREEZE && super::scorer::FREEZE_SECS == SCORE_FREEZE_SECS);
const _: () = assert!(SCORE_SUBJECT == 36 && SCORE_EVENT == core::mem::size_of::<TelemetryEvent>());

pub struct Scoring {
    scorer: Scorer,
}

/// What the scorer acts through here: it says what would happen, and the
/// records it makes go back to the runtime.
struct Remote {
    mode: Mode,
    events: Vec<TelemetryEvent>,
}

impl Enforcer for Remote {
    fn mode(&self) -> u8 {
        self.mode as u8
    }
    fn eligible(&self, who: &Subject) -> bool {
        shield::eligible_in(self.mode, who)
    }
    /// As `ShieldPolicy::trip` answers: contained only when enforcing and eligible.
    fn trip(&mut self, _now_ns: u64, who: &Subject, _shield_id: u32) -> bool {
        self.mode != Mode::Monitor && shield::eligible_in(self.mode, who)
    }
    fn emit(&mut self, ev: TelemetryEvent) {
        self.events.push(ev);
    }
}

impl Scoring {
    pub fn new() -> Scoring {
        Scoring { scorer: Scorer::new() }
    }
}

impl Extension for Scoring {
    fn handle(&mut self, req: &Request<'_>, out: &mut Vec<u8>) -> Option<(u32, u32)> {
        if req.op != OP_SCORE || req.count != 3 {
            return None;
        }
        let (head, subjects, events) = (req.fields[0], req.fields[1], req.fields[2]);
        if head.len() != SCORE_HEAD || subjects.len() % SCORE_SUBJECT != 0 || events.len() % SCORE_EVENT != 0 || events.len() / SCORE_EVENT > SCORE_EVENTS_MAX || head[2] != 0 || head[3] != 0 {
            return None;
        }
        let mode = match head[0] {
            0 => Mode::Monitor,
            1 => Mode::Enforce,
            2 => Mode::Strict,
            _ => return None,
        };
        let more = match head[1] {
            0 => false,
            1 => true,
            _ => return None,
        };
        let idle_s = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        let now_ns = u64::from_le_bytes([head[8], head[9], head[10], head[11], head[12], head[13], head[14], head[15]]);
        let subjects: Vec<Subject> = subjects.chunks(SCORE_SUBJECT).map(Subject::from_wire).collect::<Option<_>>()?;
        let events: Vec<TelemetryEvent> = events.chunks(SCORE_EVENT).map(TelemetryEvent::from_wire).collect::<Option<_>>()?;
        let mut remote = Remote { mode, events: Vec::new() };
        let locals = if more {
            for ev in &events {
                self.scorer.ingest(ev, idle_s);
            }
            Vec::new()
        } else {
            self.scorer.step(&mut remote, &events, idle_s, now_ns, &subjects)
        };
        out.extend_from_slice(&self.scorer.scored.to_le_bytes());
        out.extend_from_slice(&self.scorer.warned.to_le_bytes());
        out.extend_from_slice(&model::MODEL_ID.to_le_bytes());
        out.extend_from_slice(&[self.scorer.windows.iter().any(|w| w.pending()) as u8, locals.len() as u8, 0, 0]);
        // `step` makes one record for each result, in order.
        for (local, ev) in locals.iter().zip(&remote.events) {
            out.extend_from_slice(&local.pid.to_le_bytes());
            out.extend_from_slice(&[local.action, local.enforced as u8, local.verdict, 0]);
            out.extend_from_slice(&local.margin.to_le_bytes());
            out.extend_from_slice(&ev.to_wire());
        }
        Some((V_ALLOW, locals.len() as u32))
    }
}
