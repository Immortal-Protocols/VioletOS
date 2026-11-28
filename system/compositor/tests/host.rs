//! Host tests for the compositor (system/compositor), run by `make test-unit`:
//! the real scene rules, blits and composition code over arrays in place of
//! the framebuffer and the apps' surfaces. No files, ports or globals: every
//! test builds its own screen.
#![allow(dead_code)]

#[path = "../blit.rs"]
mod blit;
#[path = "../display.rs"]
mod display;
#[path = "../gate.rs"]
mod gate;
#[path = "../scene.rs"]
mod scene;

use display::{Compositor, Hardware, CLASSIC_H, CLASSIC_W, PIXELS, STATS_AT, TAIL_WORDS};
use scene::*;

/// A screen, sixteen built-in surfaces and a clock that advances `step`
/// ticks each time it is read (1000 ticks a millisecond).
struct Fake {
    screen: Vec<u32>,
    surfaces: Vec<Vec<u32>>,
    clock: u32,
    step: u32,
    rows_read: usize,
}

impl Fake {
    fn new() -> Fake {
        let mut seed = 0x1234_5678u32;
        let surfaces = (0..16)
            .map(|_| (0..CLASSIC_W * CLASSIC_H).map(|_| next(&mut seed)).collect())
            .collect();
        Fake { screen: vec![0xdead_0000; PIXELS + TAIL_WORDS], surfaces, clock: 0, step: 500, rows_read: 0 }
    }
    fn tail(&self) -> &[u32] {
        &self.screen[PIXELS..]
    }
}

impl Hardware for Fake {
    fn classic_row(&mut self, client: u32, y: usize, out: &mut [u32; CLASSIC_W]) {
        self.rows_read += 1;
        out.copy_from_slice(&self.surfaces[client as usize - 1][y * CLASSIC_W..(y + 1) * CLASSIC_W]);
    }
    fn scanout(&mut self) -> &mut [u32] {
        &mut self.screen
    }
    fn ticks(&mut self) -> u32 {
        self.clock = self.clock.wrapping_add(self.step);
        self.clock
    }
    fn ticks_per_ms(&mut self) -> u32 {
        1000
    }
}

fn next(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *seed ^ (*seed >> 15)
}

/// What the Display process did before it was the compositor: every pixel of
/// rows first..end three wide and four high.
fn old_display(screen: &mut [u32], surface: &[u32], first: usize, end: usize) {
    for y in first..end {
        for x in 0..240 {
            let color = surface[y * 240 + x];
            for dy in 0..4 {
                let pos = (4 * y + dy) * 720 + 3 * x;
                for dx in 0..3 {
                    screen[pos + dx] = color;
                }
            }
        }
    }
}

// ---- rectangles and damage ----

#[test]
fn rect_basics() {
    let a = Rect::new(10, 10, 20, 20);
    assert_eq!(a.area(), 100);
    assert!(Rect::new(5, 5, 5, 9).is_empty());
    assert_eq!(Rect::new(5, 5, 5, 9).area(), 0);
    assert_eq!(a.intersect(&Rect::new(15, 0, 30, 12)), Rect::new(15, 10, 20, 12));
    assert!(a.intersect(&Rect::new(20, 10, 30, 20)).is_empty());
    assert_eq!(a.bound(&Rect::new(0, 0, 0, 0)), a);
    assert_eq!(a.bound(&Rect::new(30, 5, 40, 12)), Rect::new(10, 5, 40, 20));
    // Sharing an edge counts as touching; a gap of one pixel does not.
    assert!(a.touches(&Rect::new(20, 10, 30, 20)));
    assert!(!a.touches(&Rect::new(21, 10, 30, 20)));
    assert!(a.contains(10, 10) && a.contains(19, 19));
    assert!(!a.contains(20, 10) && !a.contains(-1, 12) && !a.contains(12, -1));
}

#[test]
fn damage_covers_everything_added_and_stays_small() {
    let mut seed = 7u32;
    for round in 0..300 {
        let mut damage = Damage::new();
        // A coarse map of the screen: one cell for 8x8 pixels.
        let (cw, ch) = (W as usize / 8, H as usize / 8);
        let mut map = vec![false; cw * ch];
        for _ in 0..(1 + round % 40) {
            let x0 = next(&mut seed) % 90;
            let y0 = next(&mut seed) % 200;
            let w = 1 + next(&mut seed) % 12;
            let h = 1 + next(&mut seed) % 20;
            // Some rectangles hang over the screen's edge.
            let r = Rect::new(x0 * 8, y0 * 8, (x0 + w) * 8, (y0 + h) * 8);
            damage.add(r);
            let c = r.intersect(&SCREEN);
            for y in c.y0 / 8..c.y1 / 8 {
                for x in c.x0 / 8..c.x1 / 8 {
                    map[y as usize * cw + x as usize] = true;
                }
            }
        }
        let rects = damage.rects();
        assert!(rects.len() <= DAMAGE_RECTS);
        for (i, a) in rects.iter().enumerate() {
            assert!(!a.is_empty() && a.intersect(&SCREEN) == *a, "inside the screen: {a:?}");
            for b in &rects[i + 1..] {
                assert!(!a.touches(b), "{a:?} and {b:?} should be one");
            }
        }
        for y in 0..ch {
            for x in 0..cw {
                if map[y * cw + x] {
                    assert!(rects.iter().any(|r| r.contains(x as i32 * 8, y as i32 * 8)), "cell {x},{y} lost in round {round}");
                }
            }
        }
        assert!(damage.area() <= SCREEN.area());
    }
}

#[test]
fn damage_keeps_far_apart_changes_apart() {
    let mut d = Damage::new();
    d.add(Rect::new(0, 0, 720, 32)); // the band
    d.add(Rect::new(300, 1500, 420, 1560)); // a button near the bottom
    assert_eq!(d.rects().len(), 2);
    assert_eq!(d.area(), 720 * 32 + 120 * 60);
    // Not the 720 x 1560 a single bounding rectangle would redraw.
    let taken = d.take();
    assert!(d.is_empty() && taken.rects().len() == 2);
    // Empty and off-screen rectangles add nothing.
    d.add(Rect::new(5, 5, 5, 5));
    d.add(Rect::new(800, 0, 900, 10));
    assert!(d.is_empty());
}

// ---- the built-in apps' path, as the Display process runs it ----

#[test]
fn classic_present_matches_the_old_display_pixel_for_pixel() {
    let mut seed = 99u32;
    for client in [1u32, 2, 10, 14] {
        let mut hw = Fake::new();
        let mut c = Compositor::new();
        let mut want = hw.screen.clone();
        for _ in 0..12 {
            let first = next(&mut seed) % 400;
            let end = first + 1 + next(&mut seed) % (400 - first);
            let shown = c.present_classic(&mut hw, client, first, end).unwrap();
            assert_eq!(shown, Rect::new(0, 4 * first, 720, 4 * end));
            old_display(&mut want, &hw.surfaces[client as usize - 1].clone(), first as usize, end as usize);
            assert!(hw.screen[..PIXELS] == want[..PIXELS], "client {client} rows {first}..{end}");
        }
        // The whole surface.
        c.present_classic(&mut hw, client, 0, 400).unwrap();
        old_display(&mut want, &hw.surfaces[client as usize - 1].clone(), 0, 400);
        assert!(hw.screen[..PIXELS] == want[..PIXELS]);
    }
}

#[test]
fn classic_present_reads_each_source_row_once() {
    let mut hw = Fake::new();
    let mut c = Compositor::new();
    c.present_classic(&mut hw, 1, 100, 150).unwrap();
    assert_eq!(hw.rows_read, 50);
}

#[test]
fn classic_present_refuses_what_is_not_a_band_of_the_surface() {
    let mut hw = Fake::new();
    let mut c = Compositor::new();
    let before = hw.screen.clone();
    assert_eq!(c.present_classic(&mut hw, 1, 10, 10), Err(Refusal::Invalid));
    assert_eq!(c.present_classic(&mut hw, 1, 20, 10), Err(Refusal::Invalid));
    assert_eq!(c.present_classic(&mut hw, 1, 0, 401), Err(Refusal::Invalid));
    assert_eq!(c.present_classic(&mut hw, 0, 0, 10), Err(Refusal::NotFront));
    assert!(hw.screen == before, "a refused present drew");
    assert_eq!(c.budget.frames, 0);
}

#[test]
fn the_dirty_record_tells_the_viewer_which_rows_changed() {
    let mut hw = Fake::new();
    hw.screen[PIXELS..].fill(0);
    let mut c = Compositor::new();
    c.present_classic(&mut hw, 1, 10, 20).unwrap();
    assert_eq!(&hw.tail()[..4], &[0x5452_4944, 40, 80, 1]);
    // The viewer has not copied them yet: the next rows are merged in.
    c.present_classic(&mut hw, 1, 100, 110).unwrap();
    assert_eq!(&hw.tail()[..4], &[0x5452_4944, 40, 440, 2]);
    // The viewer caught up (it stores the sequence it copied in word 4).
    hw.screen[PIXELS + 4] = 2;
    c.present_classic(&mut hw, 1, 300, 301).unwrap();
    assert_eq!(&hw.tail()[..4], &[0x5452_4944, 1200, 1204, 3]);
    c.clear(&mut hw);
    assert_eq!(&hw.tail()[..4], &[0x5452_4944, 0, 1600, 4]);
    assert!(hw.screen[..PIXELS].iter().all(|&p| p == 0));
}

// ---- system rows are never a client's ----

fn full_surface(seed: &mut u32) -> Vec<u32> {
    (0..PIXELS).map(|_| next(seed) | 1).collect()
}

#[test]
fn no_submission_changes_a_band_row() {
    let mut seed = 5u32;
    let mut hw = Fake::new();
    let mut c = Compositor::new();
    const BAND: u32 = 32;
    c.scene.set_band(BAND);
    c.scene.set_front(9, 1, 1).unwrap();
    c.scene.damage.take();
    // The system draws its band.
    assert_eq!(c.system_fill(&mut hw, Rect::new(0, 0, W, BAND), 0xff0c_0a10), Rect::new(0, 0, W, BAND));
    let surface = full_surface(&mut seed);
    for _ in 0..200 {
        let x0 = next(&mut seed) % W;
        let y0 = next(&mut seed) % 120; // often inside or across the band
        let r = Rect::new(x0, y0, x0 + 1 + next(&mut seed) % (W - x0), y0 + 1 + next(&mut seed) % 200);
        match c.present_surface(&mut hw, 9, &surface, r) {
            Ok(shown) => {
                assert!(shown.y0 >= BAND && shown == r.intersect(&Rect::new(0, BAND, W, H)));
                for y in shown.y0..shown.y1 {
                    let (a, b) = ((y * W + shown.x0) as usize, (y * W + shown.x1) as usize);
                    assert!(hw.screen[a..b] == surface[a..b]);
                }
            }
            Err(e) => {
                assert_eq!(e, Refusal::SystemRows);
                assert!(r.y1 <= BAND);
            }
        }
        assert!(hw.screen[..(BAND * W) as usize].iter().all(|&p| p == 0xff0c_0a10), "a client drew on the band with {r:?}");
    }
    // The same for a built-in app's scaled surface: its first eight rows
    // would land on the band, and are not drawn.
    c.scene.set_front(1, 3, 4).unwrap();
    c.scene.damage.take();
    assert_eq!(c.present_classic(&mut hw, 1, 0, 8), Err(Refusal::SystemRows));
    assert_eq!(c.present_classic(&mut hw, 1, 0, 400).unwrap(), Rect::new(0, BAND, W, H));
    assert!(hw.screen[..(BAND * W) as usize].iter().all(|&p| p == 0xff0c_0a10));
    let mut want = vec![0; PIXELS];
    old_display(&mut want, &hw.surfaces[0], 0, 400);
    assert!(hw.screen[(BAND * W) as usize..PIXELS] == want[(BAND * W) as usize..]);
}

#[test]
fn nothing_is_shown_from_a_client_while_system_ui_covers_the_screen() {
    let mut seed = 11u32;
    let mut hw = Fake::new();
    let mut c = Compositor::new();
    c.scene.set_band(32);
    c.scene.set_front(9, 1, 1).unwrap();
    let surface = full_surface(&mut seed);
    c.compose_surface(&mut hw, &surface);
    assert!(hw.screen[(32 * W) as usize..PIXELS] == surface[(32 * W) as usize..]);

    // A prompt comes up: the system may draw anywhere, the client nowhere.
    c.scene.set_cover(true);
    c.compose_surface(&mut hw, &surface);
    assert_eq!(c.system_fill(&mut hw, Rect::new(40, 500, 680, 1100), 0xff22_2222), Rect::new(40, 500, 680, 1100));
    let covered = hw.screen[..PIXELS].to_vec();
    let other = full_surface(&mut seed);
    assert_eq!(c.present_surface(&mut hw, 9, &other, SCREEN), Err(Refusal::Covered));
    assert_eq!(c.present_surface(&mut hw, 9, &other, Rect::new(100, 600, 200, 700)), Err(Refusal::Covered));
    c.compose_surface(&mut hw, &other);
    assert!(hw.screen[..PIXELS] == covered[..], "a client drew over system UI");

    // It goes: the client's rows are damaged and drawn again from its surface.
    c.scene.set_cover(false);
    assert!(!c.scene.damage.is_empty());
    c.compose_surface(&mut hw, &other);
    assert!(hw.screen[(32 * W) as usize..PIXELS] == other[(32 * W) as usize..]);
    // Uncovered, the system has only its band again.
    assert_eq!(c.system_fill(&mut hw, Rect::new(40, 500, 680, 1100), 0xff22_2222), Rect::default());
    assert_eq!(c.system_fill(&mut hw, Rect::new(700, 10, 900, 100), 7), Rect::new(700, 10, 720, 32));
    assert!(hw.screen[(32 * W) as usize..PIXELS] == other[(32 * W) as usize..]);
}

#[test]
fn only_the_client_in_front_is_shown() {
    let mut seed = 3u32;
    let mut hw = Fake::new();
    let mut c = Compositor::new();
    let surface = full_surface(&mut seed);
    let before = hw.screen.clone();
    // No one is in front; then client 9 is, and client 3 is not.
    assert_eq!(c.present_surface(&mut hw, 9, &surface, SCREEN), Err(Refusal::NotFront));
    c.scene.set_front(9, 1, 1).unwrap();
    c.scene.damage.take();
    assert_eq!(c.present_surface(&mut hw, 3, &surface, SCREEN), Err(Refusal::NotFront));
    assert_eq!(c.present_surface(&mut hw, 0, &surface, SCREEN), Err(Refusal::NotFront));
    assert!(hw.screen == before);
    // A rectangle outside its surface, or a surface too short to read, is refused whole.
    assert_eq!(c.present_surface(&mut hw, 9, &surface, Rect::new(0, 0, 721, 10)), Err(Refusal::Invalid));
    assert_eq!(c.present_surface(&mut hw, 9, &surface, Rect::new(0, 1590, 10, 1601)), Err(Refusal::Invalid));
    assert_eq!(c.present_surface(&mut hw, 9, &surface[..PIXELS - 1], SCREEN), Err(Refusal::Invalid));
    assert!(hw.screen == before);
    assert!(c.present_surface(&mut hw, 9, &surface, SCREEN).is_ok());
    // A scale of nothing, or beyond MAX_SCALE, is no surface.
    assert_eq!(c.scene.set_front(4, 0, 4), Err(Refusal::Invalid));
    assert_eq!(c.scene.set_front(4, 3, 9), Err(Refusal::Invalid));
    // Coordinates chosen to overflow the bounds arithmetic are refused too.
    assert_eq!(c.present_surface(&mut hw, 9, &surface, Rect::new(0, 0, u32::MAX, 10)), Err(Refusal::Invalid));
    assert_eq!(c.present_surface(&mut hw, 9, &surface, Rect::new(0, 0, 10, 0x8000_0001)), Err(Refusal::Invalid));
    assert_eq!(c.scene.front(), 9);
    // With the screen off nothing is shown either.
    c.scene.set_power(false);
    assert_eq!(c.present_surface(&mut hw, 9, &surface, SCREEN), Err(Refusal::Off));
    assert!(c.scene.client_region().is_empty());
}

// ---- touches ----

#[test]
fn a_gesture_belongs_to_whoever_owned_the_rows_it_started_on() {
    let mut s = Scene::new();
    s.set_band(32);
    s.set_front(9, 1, 1).unwrap();
    // On the app: all of it goes to the app, even when it slides onto the band.
    assert_eq!(s.route(100, 500, true, false, false), Route::Client(9));
    assert_eq!(s.route(100, 10, true, false, false), Route::Client(9));
    assert_eq!(s.route(100, 10, false, false, false), Route::Client(9));
    // On the band: none of it reaches the app, even when it slides off.
    assert_eq!(s.route(100, 10, true, false, false), Route::System);
    assert_eq!(s.route(100, 500, true, false, false), Route::System);
    assert_eq!(s.route(100, 500, false, false, false), Route::System);
    // A lift with no finger down is nobody's.
    assert_eq!(s.route(0, 0, false, false, false), Route::Nowhere);
    // No app in front: the system (its app list) has the screen.
    s.set_front(0, 0, 0).unwrap();
    assert_eq!(s.route(100, 500, true, false, false), Route::System);
    assert_eq!(s.route(100, 500, false, false, false), Route::System);
}

#[test]
fn system_ui_takes_every_touch_and_never_the_tail_of_an_apps_gesture() {
    let mut s = Scene::new();
    s.set_band(32);
    s.set_front(9, 1, 1).unwrap();
    assert_eq!(s.route(300, 900, true, false, false), Route::Client(9));
    // A prompt appears under the finger: the app is told its gesture ended,
    // and the finger still down does not press the prompt's button.
    assert_eq!(s.set_cover(true), Some(9));
    assert_eq!(s.route(300, 900, true, false, false), Route::Nowhere);
    assert_eq!(s.route(300, 900, false, false, false), Route::Nowhere);
    // The next touch is the system's, wherever it lands.
    assert_eq!(s.route(300, 900, true, false, false), Route::System);
    // The prompt goes while the finger is down: the app does not get the lift.
    assert_eq!(s.set_cover(false), None);
    assert_eq!(s.route(300, 900, true, false, false), Route::System);
    assert_eq!(s.route(300, 900, false, false, false), Route::System);
    assert_eq!(s.route(300, 900, true, false, false), Route::Client(9));
    // Another app comes to the front under the finger.
    assert_eq!(s.set_front(3, 3, 4), Ok(Some(9)));
    assert_eq!(s.route(300, 900, true, false, false), Route::Nowhere);
    assert_eq!(s.route(300, 900, false, false, false), Route::Nowhere);
    assert_eq!(s.route(300, 900, true, false, false), Route::Client(3));
    // The screen goes off: the gesture is dropped, and the touch that wakes
    // the phone is the system's.
    s.set_power(false);
    assert_eq!(s.route(300, 900, true, false, false), Route::System);
}

#[test]
fn synthetic_touches_never_reach_system_ui() {
    let mut s = Scene::new();
    s.set_band(32);
    s.set_front(9, 1, 1).unwrap();
    // Every combination: who made it, where, and what is on screen.
    for trusted in [false, true] {
        for covered in [false, true] {
            for (x, y) in [(100, 10), (100, 500), (-5, 500), (100, 1600)] {
                s.set_cover(covered);
                let to = s.route(x, y, true, true, trusted);
                let expect = if trusted && !covered && (x, y) == (100, 500) { Route::Client(9) } else { Route::Nowhere };
                assert_eq!(to, expect, "trusted {trusted} covered {covered} at {x},{y}");
                assert_ne!(to, Route::System);
            }
        }
    }
    s.set_cover(false);
    // Not while a real finger is on system rows, and not with no app in front.
    assert_eq!(s.route(100, 10, true, false, false), Route::System);
    assert_eq!(s.route(100, 500, true, true, true), Route::Nowhere);
    assert_eq!(s.route(100, 10, false, false, false), Route::System);
    assert_eq!(s.route(100, 500, true, true, true), Route::Client(9));
    s.set_front(0, 0, 0).unwrap();
    assert_eq!(s.route(100, 500, true, true, true), Route::Nowhere);
    s.set_front(9, 1, 1).unwrap();
    s.set_power(false);
    assert_eq!(s.route(100, 500, true, true, true), Route::Nowhere);
}

// ---- the frame budget ----

#[test]
fn compositions_are_measured_against_the_budget() {
    let mut hw = Fake::new();
    hw.screen[PIXELS..].fill(0);
    let mut c = Compositor::new();
    // The fake clock moves 500 ticks a reading at 1000 a millisecond: a
    // composition reads it at its start and its end.
    c.present_classic(&mut hw, 1, 0, 100).unwrap();
    assert_eq!(c.budget.budget(), 8_000);
    assert_eq!((c.budget.frames, c.budget.over, c.budget.last), (1, 0, 500));
    hw.step = 9_000;
    c.present_classic(&mut hw, 1, 0, 400).unwrap();
    assert_eq!((c.budget.frames, c.budget.over, c.budget.last, c.budget.max), (2, 1, 9_000, 9_000));
    assert_eq!(c.budget.pixels, 720 * 400 + 720 * 1600);
    // Published after the frame for whoever measures (magic "CMPS").
    let stats = &hw.tail()[STATS_AT..STATS_AT + STATS_WORDS];
    assert_eq!(stats, &[0x5350_4d43, 1_000, 2, 1, 9_000, 9_000, 9_500, 1_406]);
    // The dirty record before it is untouched by it.
    assert_eq!(hw.tail()[0], 0x5452_4944);
    assert!(hw.tail()[5..STATS_AT].iter().all(|&w| w == 0));
    // Exactly the budget is within it; the counter may wrap during a frame.
    let mut b = Budget::new();
    b.record(u32::MAX - 3_999, 4_000, 1_000, 10);
    assert_eq!((b.last, b.over), (8_000, 0));
    b.record(0, 8_001, 1_000, 10);
    assert_eq!((b.last, b.over, b.max, b.total), (8_001, 1, 8_001, 16_001));
    b.record(0, u32::MAX, 1_000, 0);
    assert_eq!(b.total, u32::MAX);
}

#[test]
fn ticks_as_microseconds() {
    assert_eq!(ticks_us(13_000, 13_000), 1_000);
    assert_eq!(ticks_us(0, 13_000), 0);
    // QEMU's 62.5 MHz counter and the phone's 13 MHz one.
    assert_eq!(ticks_us(62_500 * 8, 62_500), 8_000);
    assert_eq!(ticks_us(6_500, 13_000), 500);
    // A rate not yet known (0) divides by nothing; a huge gap does not overflow.
    assert_eq!(ticks_us(5, 0), 5_000);
    assert_eq!(ticks_us(u32::MAX, 1), u32::MAX);
}

// ---- blits ----

#[test]
fn blits_match_the_plain_loops_at_every_length() {
    let mut seed = 21u32;
    for n in 0..200usize {
        let src: Vec<u32> = (0..n).map(|_| next(&mut seed)).collect();
        let (mut a, mut b) = (vec![1u32; n + 2], vec![1u32; n + 2]);
        blit::copy(&mut a[1..n + 1], &src);
        blit::plain::copy(&mut b[1..n + 1], &src);
        assert!(a == b && a[0] == 1 && a[n + 1] == 1, "copy {n}");
        blit::fill(&mut a[1..n + 1], 0xabcd_0000 + n as u32);
        blit::plain::fill(&mut b[1..n + 1], 0xabcd_0000 + n as u32);
        assert!(a == b && a[0] == 1 && a[n + 1] == 1, "fill {n}");
        let (mut a, mut b) = (vec![2u32; 3 * n + 2], vec![2u32; 3 * n + 2]);
        blit::widen3(&mut a[1..3 * n + 1], &src);
        blit::plain::widen3(&mut b[1..3 * n + 1], &src);
        assert!(a == b && a[0] == 2 && a[3 * n + 1] == 2, "widen3 {n}");
        for (i, p) in src.iter().enumerate() {
            assert_eq!(&a[1 + 3 * i..4 + 3 * i], &[*p; 3]);
        }
    }
    // The shorter side decides.
    let mut d = [0u32; 4];
    blit::copy(&mut d, &[1, 2, 3, 4, 5, 6]);
    assert_eq!(d, [1, 2, 3, 4]);
    blit::widen3(&mut d, &[9, 8, 7]);
    assert_eq!(d, [9, 9, 9, 4]);
}

// ---- locked: what Runtime refuses (gate.rs) ----

#[test]
fn locked_nothing_else_comes_to_the_front_or_changes_the_system() {
    use gate::*;
    const GET: u32 = 2;
    // runtime/protocol.rs: FOCUS, SAVE, RADIO, POWER, FILE_WRITE.
    let (focus, save, radio, power, file_write) = (3u32, 5u32, 7u32, 8u32, 23u32);
    for from_open in [false, true] {
        // The focus cannot move, whoever asks.
        assert!(locked_refuses(EXECUTE, OPEN, from_open));
        assert!(locked_refuses(EXECUTE, QUIT, from_open));
        // Reading is never refused, nor are files, sleep and wake, or the
        // radio switch (the launcher's one-shot restore and USB Wi-Fi jobs).
        for key in [OPEN, QUIT, INSTALL, REMOVE, radio, RELAY, PRESENT, MICROPHONE, focus, power] {
            assert!(!locked_refuses(GET, key, from_open));
        }
        for key in [power, save, file_write, focus, radio] {
            assert!(!locked_refuses(EXECUTE, key, from_open));
        }
    }
    // Anyone but the Open Runtime (whose lock screen it is) draws nothing,
    // makes no sound, listens to nothing, and installs, removes and relays
    // nothing; the Open Runtime's own requests go on.
    for key in [PRESENT, VOLUME, PLAYBACK, MICROPHONE, INSTALL, REMOVE, RELAY] {
        assert!(locked_refuses(EXECUTE, key, false), "key {key}");
        assert!(!locked_refuses(EXECUTE, key, true), "key {key}");
    }
}
