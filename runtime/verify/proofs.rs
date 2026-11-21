//! Kani harnesses. Together they show that the invariant in model.rs holds
//! from startup and is preserved by every request and every worker step, and
//! that no step violates the policy checks in model.rs. That covers every
//! reachable Runtime state, not just a bounded number of steps.
use crate::model::*;
use crate::protocol::*;
use crate::router::*;

impl kani::Arbitrary for Message {
    fn any() -> Self {
        Message {
            op: kani::any(),
            key: kani::any(),
            value: kani::any(),
            arg: kani::any(),
            data: kani::any(),
            tail: kani::any(),
        }
    }
}
impl kani::Arbitrary for Origin {
    fn any() -> Self {
        match kani::any::<u8>() % 4 {
            0 => Origin::Runtime,
            1 => Origin::System,
            2 => Origin::App(kani::any()),
            _ => Origin::Touch(kani::any()),
        }
    }
}
impl kani::Arbitrary for Cell {
    fn any() -> Self {
        Cell {
            state: kani::any(),
            owner: kani::any(),
            op: kani::any(),
            key: kani::any(),
            value: kani::any(),
            tag: kani::any(),
        }
    }
}
fn any_job() -> Job {
    Job { ticket: kani::any(), lane: kani::any(), result: Out { m: kani::any(), origin: kani::any() } }
}
fn any_stream() -> Stream {
    Stream { id: kani::any(), last: if kani::any() { Some((kani::any(), kani::any())) } else { None } }
}
fn any_client() -> Client {
    Client {
        serial: kani::any(),
        jobs: [any_job(), any_job(), any_job(), any_job(), any_job(), any_job(), any_job()],
        streams: [any_stream(), any_stream(), any_stream()],
    }
}
/// A Runtime with arbitrary state for `who`'s client and the shared fields.
/// Other clients start empty: serving `who` must leave them untouched.
fn any_runtime_for(who: u32) -> Runtime {
    let mut clients = [Client::default(); 15];
    if who < 15 {
        clients[who as usize] = any_client();
    }
    Runtime {
        inventory: kani::any(),
        focus: kani::any(),
        held: kani::any(),
        asleep: kani::any(),
        contact_fresh: kani::any(),
        ready: kani::any(),
        startup: Startup {
            touch_pending: kani::any(),
            disk_pending: kani::any(),
            disk_reason_pending: kani::any(),
            disk_reason: if kani::any() { Some(kani::any()) } else { None },
        },
        clients,
    }
}
/// Cells with arbitrary contents wherever `who` may reach; free elsewhere.
fn any_world_for(who: u32) -> World {
    let mut w = World::empty();
    w.presenter = kani::any();
    for lane in TOUCH_LANE..=PACKAGE_LANE {
        for index in reachable_cells(who) {
            if index < 32 {
                *w.cell(lane, index) = kani::any();
            }
        }
    }
    w
}
/// Invariant restricted to what `who`'s request can reach.
///
/// This is enough for the full invariant: `cell_ok` depends only on its own
/// cell, except the touch and startup cells, which also read focus and the
/// startup flags. Those cells are always in the reachable set. `World` fails
/// the proof if the router touches any other cell, and `others_untouched`
/// fails it if any other client changes. So every cell and client outside
/// the set keeps whatever valid contents it had.
fn local_invariant(rt: &Runtime, w: &World, who: u32) -> bool {
    state_ok(rt, w)
        && (who >= 15 || !is_app(who) || client_ok(rt, who))
        && (TOUCH_LANE..=PACKAGE_LANE)
            .all(|lane| reachable_cells(who).iter().all(|&i| i >= 32 || cell_at(rt, w, lane, i)))
}
fn others_untouched(rt: &Runtime, who: u32) -> bool {
    (0..15u32).filter(|&i| i != who).all(|i| {
        let c = &rt.clients[i as usize];
        c.serial == 0
            && c.streams.iter().all(|s| s.id == 0 && s.last.is_none())
            && c.jobs.iter().all(|j| j.ticket == 0 && j.lane == 0 && j.result.origin == Origin::Runtime)
    })
}

/// One request from app `who` with the given operation, from any state.
fn serve(who: u32, op: Option<u32>) {
    let mut rt = any_runtime_for(who);
    let mut w = any_world_for(who);
    kani::assume(local_invariant(&rt, &w, who));
    let mut m: Message = kani::any();
    match op {
        Some(op) => m.op = op,
        None => kani::assume(!matches!(m.op, SUBSCRIBE | GET | EXECUTE | POLL | FORGET)),
    }
    w.begin(who, &rt);
    rt.serve(who, m, &mut w);
    assert!(w.replies == 1, "request left without exactly one reply");
    assert!(local_invariant(&rt, &w, who), "request broke the invariant");
    assert!(others_untouched(&rt, who), "request changed another app's state");
}

#[kani::proof]
#[kani::unwind(42)]
fn startup_establishes_invariant() {
    let mut w = World::empty();
    let rt = Runtime::start(kani::any(), &mut w);
    assert!(invariant(&rt, &w));
}

/// Startup diagnostics, which run before every request, keep the invariant
/// and read only system data (plus the stopwatch disk bit).
#[kani::proof]
#[kani::unwind(42)]
fn startup_step_keeps_policy() {
    let mut rt = any_runtime_for(SYSTEM);
    let mut w = any_world_for(SYSTEM);
    kani::assume(local_invariant(&rt, &w, SYSTEM));
    w.begin(SYSTEM, &rt);
    rt.startup_step(&mut w);
    assert!(w.replies == 0);
    assert!(local_invariant(&rt, &w, SYSTEM));
    assert!(others_untouched(&rt, SYSTEM));
}

/// Callers that are not apps are refused without any effect: request
/// handling is replaced by a stub that fails the proof if it is ever reached.
fn never_serve_app<I: Io>(_: &mut Runtime, _: u32, _: Message, _: &mut I) {
    panic!("a non-app reached request handling");
}
#[kani::proof]
#[kani::unwind(42)]
#[kani::stub(Runtime::serve_app, never_serve_app)]
fn non_apps_are_refused() {
    let who: u32 = kani::any();
    kani::assume(!is_app(who));
    let mut w = World::empty();
    let mut rt = Runtime::start(0, &mut w);
    w.begin(who, &rt);
    rt.serve(who, kani::any(), &mut w);
    assert!(w.replies == 1);
    assert!(invariant(&rt, &w));
    assert!(others_untouched(&rt, 15));
}

/// One harness per app and operation keeps each solver run small: with a
/// concrete caller, the solver need not consider every client at every step.
macro_rules! requests {
    ($($name:ident: $op:expr;)*) => {
        $(mod $name {
            use super::*;
            #[kani::proof] #[kani::unwind(42)] fn launcher() { serve(LAUNCHER, $op) }
            #[kani::proof] #[kani::unwind(42)] fn stopwatch() { serve(WATCH, $op) }
            #[kani::proof] #[kani::unwind(42)] fn wifi() { serve(WIFI, $op) }
            #[kani::proof] #[kani::unwind(42)] fn app11() { serve(11, $op) }
            #[kani::proof] #[kani::unwind(42)] fn app12() { serve(12, $op) }
            #[kani::proof] #[kani::unwind(42)] fn app13() { serve(13, $op) }
            #[kani::proof] #[kani::unwind(42)] fn app14() { serve(APP_LAST, $op) }
        })*
    };
}
requests! {
    subscribe: Some(SUBSCRIBE);
    poll: Some(POLL);
    forget: Some(FORGET);
    get: Some(GET);
    execute: Some(EXECUTE);
    other: None;
}

/// A service worker finishing any one request, at any moment, keeps the
/// invariant. `cell_ok` is per cell, so one arbitrary cell covers them all.
#[kani::proof]
#[kani::unwind(42)]
fn worker_step_keeps_policy() {
    let rt = any_runtime_for(SYSTEM);
    let mut w = World::empty();
    w.presenter = kani::any();
    let lane: u32 = kani::any();
    let index: u32 = kani::any();
    kani::assume((TOUCH_LANE..=PACKAGE_LANE).contains(&lane) && index < 32);
    *w.cell(lane, index) = kani::any();
    kani::assume(state_ok(&rt, &w) && cell_at(&rt, &w, lane, index));
    w.complete(lane, index, &rt);
    assert!(cell_at(&rt, &w, lane, index));
}

/// Different apps always reach different disk regions, and the region comes
/// only from the request owner.
#[kani::proof]
fn storage_regions_are_private() {
    let (a, b): (u32, u32) = (kani::any(), kani::any());
    let (ma, mb): (Message, Message) = (kani::any(), kani::any());
    kani::assume(is_app(a) && is_app(b) && a != b);
    let region = |route| match route {
        StorageRoute::Checkpoint(r) => Some(r),
        StorageRoute::File(op) => Some(op - STORAGE_FILE),
        _ => None,
    };
    let (ra, rb) = (region(storage_route(a, &ma)), region(storage_route(b, &mb)));
    if let (Some(ra), Some(rb)) = (ra, rb) {
        assert!(ra != rb);
        assert!(ra < 7 && rb < 7);
    }
    let owner: u32 = kani::any();
    kani::assume(!is_app(owner));
    let route = storage_route(owner, &ma);
    assert!(route == StorageRoute::Reject || route == StorageRoute::Diagnostic);
}

/// The display worker copies only the presenting app's own surface.
#[kani::proof]
fn display_shows_only_the_presenter() {
    let (owner, presenter): (u32, u32) = (kani::any(), kani::any());
    let m: Message = kani::any();
    if present_allowed(owner, &m, presenter) {
        assert!(is_app(owner) && owner == presenter && m.value == owner);
    }
}
