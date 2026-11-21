//! Concrete scenarios run against the same model. The model's assertions
//! fire on any policy violation, so each test also checks the policy.
use crate::model::*;
use crate::protocol::*;
use crate::router::*;

fn start() -> (Runtime, World) {
    let mut w = World::empty();
    let rt = Runtime::start(1 << STORAGE_LANE | 1 << RADIO_LANE | 1 << POWER_LANE, &mut w);
    assert!(invariant(&rt, &w));
    (rt, w)
}
fn send(rt: &mut Runtime, w: &mut World, who: u32, op: u32, key: u32, value: u32) {
    w.begin(who, rt);
    rt.handle(who, Message::new(op, key, value), w);
    assert_eq!(w.replies, 1);
    assert!(invariant(rt, w));
}
fn tap() -> Message {
    let mut m = Message::result(100);
    m.arg = 200;
    m.key = 1; // finger down
    m
}

/// Before this change, a tap sampled for the launcher could be handed to the
/// stopwatch if focus switched while the sample was in flight.
#[test]
fn touch_sampled_for_one_app_is_not_delivered_to_the_next() {
    let (mut rt, mut w) = start();
    send(&mut rt, &mut w, LAUNCHER, SUBSCRIBE, CONTACT, 0);
    let stream = rt.clients[LAUNCHER as usize].streams[0].id;
    send(&mut rt, &mut w, LAUNCHER, POLL, 0, stream);
    assert_eq!(w.cell(TOUCH_LANE, CONTACT_CELL).state, 1);
    w.complete(TOUCH_LANE, CONTACT_CELL, &rt);
    send(&mut rt, &mut w, LAUNCHER, EXECUTE, OPEN, WATCH);
    assert_eq!(rt.focus, WATCH);

    send(&mut rt, &mut w, WATCH, SUBSCRIBE, CONTACT, 0);
    let stream = rt.clients[WATCH as usize].streams[0].id;
    w.next = tap();
    send(&mut rt, &mut w, WATCH, POLL, 0, stream);
    // The stale sample was discarded and a fresh one requested.
    assert_eq!(w.cell(TOUCH_LANE, CONTACT_CELL).state, 1);
    assert_eq!(w.cell(TOUCH_LANE, CONTACT_CELL).owner, WATCH);
    w.complete(TOUCH_LANE, CONTACT_CELL, &rt);
    send(&mut rt, &mut w, WATCH, POLL, 0, stream);
    assert!(rt.held, "the stopwatch received its own tap");
}

#[test]
fn background_app_cannot_present() {
    let (mut rt, mut w) = start();
    send(&mut rt, &mut w, WATCH, EXECUTE, PRESENT, 400);
    assert_eq!(w.cell(DISPLAY_LANE, WATCH).state, 0);
}

#[test]
fn present_queued_before_a_focus_change_is_dropped() {
    let (mut rt, mut w) = start();
    send(&mut rt, &mut w, LAUNCHER, EXECUTE, OPEN, WATCH);
    send(&mut rt, &mut w, WATCH, EXECUTE, PRESENT, 400);
    assert_eq!(w.cell(DISPLAY_LANE, WATCH).state, 1);
    send(&mut rt, &mut w, WATCH, EXECUTE, OPEN, LAUNCHER);
    assert!(!w.complete(DISPLAY_LANE, WATCH, &rt), "stale present reached the screen");
}

#[test]
fn radio_reply_returns_only_to_its_requester() {
    let (mut rt, mut w) = start();
    send(&mut rt, &mut w, WIFI, EXECUTE, RADIO_TEST, 0);
    let ticket = rt.clients[WIFI as usize].jobs[4].ticket;
    w.complete(RADIO_LANE, WIFI, &rt);
    // Another app polling the same ticket number finds nothing.
    send(&mut rt, &mut w, WATCH, POLL, 0, ticket);
    assert_eq!(w.cell(RADIO_LANE, WIFI).state, 2);
    send(&mut rt, &mut w, WIFI, POLL, 0, ticket);
    assert_eq!(w.cell(RADIO_LANE, WIFI).state, 0);
}

#[test]
fn network_requests_are_limited_to_wifi_setup() {
    let (mut rt, mut w) = start();
    for key in [NETWORK, PROFILE, RADIO_TEST, SCAN] {
        send(&mut rt, &mut w, WATCH, GET, key, 0);
        send(&mut rt, &mut w, WATCH, EXECUTE, key, 0);
    }
    assert_eq!(w.cell(RADIO_LANE, WATCH).state, 0);
}

#[test]
fn non_apps_are_denied() {
    let (mut rt, mut w) = start();
    for who in [0, 3, 9, 15, 31, u32::MAX] {
        send(&mut rt, &mut w, who, GET, FILE_READ, 0);
    }
}

#[test]
fn storage_route_uses_the_owner_region() {
    let m = Message::new(GET, FILE_READ, 0);
    assert_eq!(storage_route(WATCH, &m), StorageRoute::File(STORAGE_FILE));
    assert_eq!(storage_route(WIFI, &m), StorageRoute::File(STORAGE_FILE + 2));
    assert_eq!(storage_route(3, &m), StorageRoute::Reject);
}
