//! A model of everything around the Runtime router: the request cells shared
//! with the service workers, the supervisor, and the replying app.
//!
//! Each cell carries a ghost `tag` saying whose data its result holds. Every
//! effect the router performs is checked against the policy as it happens.
use crate::protocol::*;
use crate::router::*;

#[derive(Clone, Copy, Default, Debug)]
pub struct Cell {
    /// 0 free, 1 requested, 2 completed (the lanes.rs protocol).
    pub state: u8,
    pub owner: u32,
    pub op: u32,
    pub key: u32,
    pub value: u32,
    pub tag: Origin,
}

pub struct World {
    pub cells: [[Cell; 32]; 6],
    /// Last value Runtime published to the display worker.
    pub presenter: u32,
    /// The request being handled and the Runtime state it arrived in.
    pub who: u32,
    pub focus: u32,
    pub asleep: bool,
    pub replies: u32,
    /// Test builds: the contents of the next worker or supervisor reply.
    pub next: Message,
    pub lifecycle_ok: bool,
}

impl World {
    pub fn empty() -> Self {
        Self {
            cells: [[Cell::default(); 32]; 6],
            presenter: 0,
            who: 0,
            focus: LAUNCHER,
            asleep: false,
            replies: 0,
            next: Message::default(),
            lifecycle_ok: true,
        }
    }
    pub fn cell(&mut self, lane: u32, index: u32) -> &mut Cell {
        assert!((TOUCH_LANE..=PACKAGE_LANE).contains(&lane) && index < 32, "cell out of range");
        &mut self.cells[(lane - TOUCH_LANE) as usize][index as usize]
    }
    /// Record the request about to be handled.
    pub fn begin(&mut self, who: u32, rt: &Runtime) {
        self.who = who;
        self.focus = rt.focus;
        self.asleep = rt.asleep;
        self.replies = 0;
    }
    fn result(&self) -> Message {
        #[cfg(kani)]
        return kani::any();
        #[cfg(not(kani))]
        self.next
    }

    /// A service worker finishes the request in one cell. A touch sample is
    /// read at this moment, so it belongs to whichever app is in front now.
    /// Returns true if the display worker would copy pixels.
    pub fn complete(&mut self, lane: u32, index: u32, rt: &Runtime) -> bool {
        let presenter = self.presenter;
        let c = self.cell(lane, index);
        if c.state != 1 {
            return false;
        }
        c.state = 2;
        if lane == TOUCH_LANE && c.key != STATUS {
            c.tag = Origin::Touch(rt.focus);
        }
        if lane != DISPLAY_LANE {
            return false;
        }
        let mut m = Message::new(c.op, c.key, c.value);
        m.arg = 0;
        let shown = present_allowed(c.owner, &m, presenter);
        if shown {
            assert!(c.owner == rt.presenter(), "pixels from an app not in front");
            assert!(c.value == c.owner, "an app presented another app's surface");
        }
        shown
    }

    /// While serving an app, Runtime touches only that app's own cells and
    /// the shared touch, wake and startup cells. The proofs rely on this to
    /// leave every other cell out of the solver's state.
    fn reachable(&self, lane: u32, index: u32) {
        let who = self.who;
        let own = is_app(who)
            && (index == who || index == stream_cell(who) || index == CONTACT_CELL
                || (lane == POWER_LANE && index == 0));
        assert!(own || index == BOOT_SLOT, "Runtime touched another app's request cell");
    }

    /// The service-access policy: which cells an app's request may occupy and
    /// which services it may reach at all.
    fn check_request(&self, lane: u32, index: u32, owner: u32, m: &Message) {
        let who = self.who;
        if index != BOOT_SLOT {
            assert!(is_app(who), "request made for a non-app caller");
        }
        match lane {
            TOUCH_LANE => assert!(
                (index == CONTACT_CELL && owner == who && who == self.focus && m.key == CONTACT)
                    || (index == BOOT_SLOT && owner == SYSTEM && m.key == STATUS),
                "touch sampled for an app not in front"
            ),
            DISPLAY_LANE => assert!(
                index == who && owner == who && who == self.focus && !self.asleep
                    && m.op == EXECUTE && m.key == PRESENT && m.value == who,
                "present by an app not in front, or of another surface"
            ),
            STORAGE_LANE => assert!(
                (index == who && owner == who
                    && matches!(m.key, SAVE | FILE_LIST | FILE_READ | FILE_WRITE | FILE_TRUNCATE | FILE_DELETE))
                    || (index == BOOT_SLOT
                        && ((owner == WATCH && m.op == GET && m.key == SAVE)
                            || (owner == SYSTEM && m.key == STATUS))),
                "storage request not bound to the caller"
            ),
            RADIO_LANE => {
                assert!(
                    (index == who || index == stream_cell(who)) && owner == who,
                    "radio request not bound to the caller"
                );
                if matches!(m.key, NETWORK | PROFILE | RADIO_TEST | SCAN) {
                    assert!(who == WIFI, "network access outside Wi-Fi Setup");
                }
                if m.op == EXECUTE && m.key == RADIO {
                    assert!(who == LAUNCHER, "radio switched by a non-launcher app");
                }
            }
            POWER_LANE => {
                assert!(
                    (index == who || index == stream_cell(who) || index == 0) && owner == who,
                    "power request not bound to the caller"
                );
                if m.op == EXECUTE {
                    assert!(
                        who == LAUNCHER || (index == 0 && m.value == 0 && who == self.focus),
                        "power changed by a non-launcher app"
                    );
                }
            }
            PACKAGE_LANE => assert!(
                index == who && owner == who && who == LAUNCHER,
                "package access outside the launcher"
            ),
            _ => panic!("unknown lane"),
        }
    }
}

impl Io for World {
    fn queue(&mut self, lane: u32, index: u32, owner: u32, m: Message) -> bool {
        self.reachable(lane, index);
        self.check_request(lane, index, owner, &m);
        let c = self.cell(lane, index);
        if c.state != 0 {
            return false;
        }
        let tag = if (lane == TOUCH_LANE || lane == STORAGE_LANE) && m.key == STATUS {
            Origin::System
        } else if is_app(owner) {
            Origin::App(owner)
        } else {
            Origin::System
        };
        *c = Cell { state: 1, owner, op: m.op, key: m.key, value: m.value, tag };
        true
    }
    fn take(&mut self, lane: u32, index: u32) -> Option<Out> {
        self.reachable(lane, index);
        let m = self.result();
        let c = self.cell(lane, index);
        if c.state != 2 {
            return None;
        }
        c.state = 0;
        Some(Out { m, origin: c.tag })
    }
    fn lifecycle(&mut self, m: Message) -> Message {
        let who = self.who;
        let open = m.op == EXECUTE && m.key == OPEN
            && ((who == LAUNCHER && is_app(m.value)) || (is_app(who) && m.value == LAUNCHER));
        let quit = m.op == EXECUTE && m.key == QUIT && m.value == who && is_app(who) && who != LAUNCHER;
        assert!(open || quit, "app switch outside the launcher rules");
        #[cfg(kani)]
        return kani::any();
        #[cfg(not(kani))]
        Message::error(if self.lifecycle_ok { OK } else { DENIED })
    }
    fn supervisor_status(&mut self) -> Message {
        assert!(self.who == LAUNCHER, "update record read outside the launcher");
        self.result()
    }
    fn heartbeat(&mut self, _: u32, _: u32) {}
    fn publish_status(&mut self, _: &Message) {}
    fn publish_presenter(&mut self, app: u32) {
        self.presenter = app;
    }
    fn reply(&mut self, out: Out) {
        assert!(self.replies == 0, "second reply to one request");
        self.replies += 1;
        match out.origin {
            Origin::Runtime => {}
            Origin::App(app) => assert!(app == self.who, "reply carries another app's service data"),
            Origin::Touch(app) => assert!(app == self.who, "touch read while another app was in front"),
            Origin::System => panic!("raw system reply"),
        }
    }
}

fn apps() -> [u32; 7] {
    [LAUNCHER, WATCH, WIFI, 11, 12, 13, APP_LAST]
}

/// Which cells may be busy, and whose data each holds.
pub fn cell_ok(rt: &Runtime, lane: u32, index: u32, c: &Cell) -> bool {
    if c.state > 2 {
        return false;
    }
    if c.state == 0 {
        return true;
    }
    let stream = index > CONTACT_CELL && index < BOOT_SLOT && is_app(index - CONTACT_CELL);
    if is_app(index) {
        return (DISPLAY_LANE..=PACKAGE_LANE).contains(&lane) && c.owner == index && c.tag == Origin::App(index);
    }
    if stream {
        let app = index - CONTACT_CELL;
        return (lane == RADIO_LANE || lane == POWER_LANE) && c.owner == app && c.tag == Origin::App(app);
    }
    match (lane, index) {
        (POWER_LANE, 0) => is_app(c.owner) && c.tag == Origin::App(c.owner),
        (TOUCH_LANE, CONTACT_CELL) => {
            is_app(c.owner) && c.key == CONTACT
                && (c.state != 2 || !rt.contact_fresh || c.tag == Origin::Touch(rt.focus))
        }
        (TOUCH_LANE, BOOT_SLOT) => c.owner == SYSTEM && c.key == STATUS && c.tag == Origin::System,
        (STORAGE_LANE, BOOT_SLOT) => {
            let diagnostic = c.owner == SYSTEM && c.key == STATUS && c.tag == Origin::System;
            let probe = c.owner == WATCH && c.key == SAVE && c.tag == Origin::App(WATCH);
            (diagnostic || probe)
                && (!rt.startup.disk_reason_pending || diagnostic)
                && (!rt.startup.disk_pending || probe)
        }
        _ => false,
    }
}

/// The parts of the invariant that are not about one cell or one client.
pub fn state_ok(rt: &Runtime, w: &World) -> bool {
    is_app(rt.focus)
        && w.presenter == rt.presenter()
        && !(rt.startup.disk_pending && rt.startup.disk_reason_pending)
}
/// Each pending job points at its own lane and holds only this app's data.
pub fn client_ok(rt: &Runtime, app: u32) -> bool {
    rt.clients[app as usize].jobs.iter().enumerate().all(|(slot, job)| {
        (job.lane == 0 || (slot >= 2 && job.lane == slot as u32 + 3))
            && (job.result.origin == Origin::Runtime || job.result.origin == Origin::App(app))
    })
}
pub fn cell_at(rt: &Runtime, w: &World, lane: u32, index: u32) -> bool {
    cell_ok(rt, lane, index, &w.cells[(lane - TOUCH_LANE) as usize][index as usize])
}

/// The inductive invariant connecting Runtime's state to the cells.
pub fn invariant(rt: &Runtime, w: &World) -> bool {
    state_ok(rt, w)
        && apps().iter().all(|&app| client_ok(rt, app))
        && (TOUCH_LANE..=PACKAGE_LANE).all(|lane| (0..32).all(|index| cell_at(rt, w, lane, index)))
}

/// The cells Runtime may touch while serving `who` (see `World::reachable`).
pub fn reachable_cells(who: u32) -> [u32; 5] {
    [who, stream_cell(who), CONTACT_CELL, BOOT_SLOT, 0]
}
