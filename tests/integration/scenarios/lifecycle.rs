//! Background lifecycle and the low-memory killer: what
//! targets/qemu/test_lifecycle.py checked with the Ticker, Jobs, Cache and
//! Pressure apps, against the shipped lifecycle.rs and procmgr.rs with the
//! fake clock and a 16 MiB page pool. No test here waits: 20 s pass in
//! microseconds.
use crate::open::front::Touched;
use crate::open::lifecycle::{Pressure, State, EXIT_CLOSED};
use crate::open::vma::Access::Write;
use crate::world::*;

const RW: u32 = 3;
const ANON: u32 = 0x22;
const PAGE: u32 = crate::kernel::sys::PAGE as u32;

fn phone() -> World {
    World::boot(Image::new().app("ticker", "").app("jobs", "").app("cache", "").app("pressure", ""))
}
/// The app maps `pages` pages and writes to each: resident memory.
fn allocate(w: &mut World, i: usize, pages: u32) -> Result<u32, Touched> {
    let base = w.app(i).mmap(0, pages * PAGE, RW, ANON, -1).expect("address space");
    for n in 0..pages {
        match w.app(i).touch(base + n * PAGE, Write) {
            Touched::Ok => {}
            other => return Err(other),
        }
    }
    Ok(base)
}

#[test]
fn an_app_left_behind_home_runs_through_its_grace_then_freezes() {
    let mut w = phone();
    let ticker = w.open_app("ticker");
    assert_eq!(w.state(ticker), State::Foreground);
    assert_eq!(w.app(ticker).sys("home"), "ok");
    // 5 s of grace, 10 s cached, then frozen: about 15 s (the QEMU test allowed 10 to 25).
    w.advance_secs(4);
    assert_eq!(w.state(ticker), State::Visible);
    w.advance_secs(2);
    assert_eq!(w.state(ticker), State::Cached, "still running: it ticks while cached");
    w.advance_secs(8);
    assert_eq!(w.state(ticker), State::Cached);
    w.advance_secs(2);
    assert_eq!(w.state(ticker), State::Frozen);
    assert_eq!(w.logged("lifecycle: froze ticker"), 1);
    w.advance_secs(60);
    assert_eq!(w.state(ticker), State::Frozen);
    assert_eq!(w.logged("lifecycle: froze ticker"), 1);
    // Opened again it runs at once.
    w.front(Some(ticker));
    assert_eq!(w.state(ticker), State::Foreground);
    // Home itself never freezes, however long it is behind.
    let home = w.rt.home.unwrap();
    w.advance_secs(120);
    assert_eq!(w.state(home), State::Visible);
}

#[test]
fn the_child_of_the_app_in_front_is_never_frozen() {
    let mut w = phone();
    let jobs = w.open_app("jobs");
    let child = w.fork(jobs);
    w.advance_secs(20);
    assert_ne!(w.state(child), State::Frozen, "the parent's waitpid would never end");
    assert_eq!(w.state(child), State::Visible);
    assert_eq!(w.logged("lifecycle: froze jobs"), 0);
    // Once its parent is behind Home, the child is cached and frozen with it.
    w.front(None);
    w.advance_secs(16);
    assert_eq!((w.state(jobs), w.state(child)), (State::Frozen, State::Frozen));
}

#[test]
fn an_app_with_a_reason_to_run_stays_visible() {
    // Audio open (lifecycle.rs `Facts::reason`): music keeps playing behind Home.
    let mut w = World::boot(Image::new().app("music", "caps: audio-out\n").app("ticker", ""));
    let music = w.open_app("music");
    assert!(w.app(music).open("/dev/violet/audio", O_RDWR).is_ok());
    let ticker = w.open_app("ticker");
    w.front(None);
    w.advance_secs(30);
    assert_eq!(w.state(music), State::Visible);
    assert_eq!(w.state(ticker), State::Frozen);
}

#[test]
fn low_memory_closes_the_frozen_app_not_the_one_in_front_or_home() {
    let mut w = phone();
    let home = w.rt.home.unwrap();
    // Cache holds memory and freezes behind Home.
    let cache = w.open_app("cache");
    allocate(&mut w, cache, 96).expect("cache resident");
    w.front(None);
    w.advance_secs(16);
    assert_eq!(w.state(cache), State::Frozen);
    let ticker = w.open_app("ticker");
    w.front(None);
    // Pressure, in front, allocates past the free pool.
    let pressure = w.open_app("pressure");
    let free = w.rt.pool.available() as u32;
    let mark = w.mark();
    allocate(&mut w, pressure, free + 40).expect("the foreground allocation survives");
    let log = w.since(mark);
    let (low, closed) = (log.find("lifecycle: app memory low").expect("background apps are told"), log.find("closed cache in the background").expect("cache is closed"));
    assert!(low < closed, "told before anything is closed:\n{log}");
    assert!(log.contains(&format!("cache (pid {}) exited with status {EXIT_CLOSED}", 101)), "{log}");
    assert!(!w.running(cache));
    assert!(w.running(pressure) && w.running(home), "Home and the app in front survive");
    assert!(!log.contains("home (pid") && !log.contains("pressure (pid"));
    // The apps behind were asked to free memory; the app in front was not.
    assert_eq!(w.rt.apps[ticker].as_ref().unwrap().low_memory, 1);
    assert_eq!(w.rt.apps[home].as_ref().unwrap().low_memory, 1);
    assert_eq!(w.rt.apps[pressure].as_ref().unwrap().low_memory, 0);
    assert_ne!(w.rt.life.pressure, Pressure::Normal);
    assert_eq!(w.rt.life.total_closed, 1);
    // Pressure ends; Cache opens again from scratch.
    assert_eq!(w.app(pressure).syscall(248, [0; 6]), 0);
    w.front(None);
    w.advance_secs(2);
    assert_eq!(w.rt.life.pressure, Pressure::Normal);
    let again = w.open_app("cache");
    assert_ne!(w.app(again).pid(), 101, "a new process");
    assert!(allocate(&mut w, again, 96).is_ok(), "cache resident again");
}

#[test]
fn an_app_behind_that_runs_out_of_memory_is_closed_itself() {
    // lifecycle.rs `fault_victim`: an app that is not in front may close
    // only frozen and cached apps; with none, it gets no page.
    let mut w = phone();
    let hog = w.open_app("pressure");
    let front = w.open_app("ticker");
    let free = w.rt.pool.available() as u32;
    assert_eq!(allocate(&mut w, hog, free + 8), Err(Touched::Ended(EXIT_CLOSED)));
    assert!(!w.running(hog) && w.running(front) && w.running(w.rt.home.unwrap()));
    assert_eq!(w.logged("closed pressure in the background"), 1);
}

#[test]
fn a_closed_apps_memory_and_sockets_come_back() {
    let mut w = World::boot(Image::new().app("cache", "caps: net\n"));
    let before = w.rt.pool.available();
    let cache = w.open_app("cache");
    allocate(&mut w, cache, 64).unwrap();
    let pid = w.app(cache).pid();
    for _ in 0..3 {
        w.app(cache).socket(2, 1).unwrap();
    }
    assert_eq!(w.rt.slots.held(pid), 3);
    assert!(w.rt.pool.available() < before - 64);
    w.rt.close_for_memory(cache);
    assert_eq!(w.rt.pool.available(), before, "every page is back in the pool");
    assert_eq!(w.rt.slots.held(pid), 0);
}
