//! A retained compositor for scrolling content (docs/8ms-frame-pipeline-rfc.md,
//! phase 4).
//!
//! The app paints a tall band of its content once into a layer; a compositor
//! thread owns the screen, follows drags straight from the touch stream, runs
//! the fling, and copies the visible window of the layer to the screen at each
//! VSync. Scrolling stays at the display's rate however long the app's main
//! thread is busy (laying out a page, waiting on the network), because no
//! scroll frame needs it. The app hears about scrolling as events: SCROLL when
//! the motion settles, BAND when the view nears the edge of what the layer
//! holds and the app should paint the next band.
//!
//! ```ignore
//! let (comp, events) = Compositor::start(Screen::open()?)?;
//! let view = comp.add_view(Rect::new(0, 120, W as i32, H as i32));
//! comp.set_layer(view, layer, origin, max_scroll);   // a Canvas painted off-lock
//! comp.chrome(|c| { /* toolbar, outside the views */ Rect::new(0, 0, W as i32, 120) });
//! ```
//! Drawing outside the views goes through `chrome`, which hands out the
//! screen canvas and presents the rectangle it returns; the compositor thread
//! is the only one that presents.
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crate::canvas::{Canvas, Color, Rect};
use crate::device::{Event, Events, Screen, H, TOUCH_DOWN, TOUCH_MOVE, TOUCH_UP, W};

/// Event kinds the compositor adds to the stream (`code` is the view).
/// SCROLL: motion settled at `value`. BAND: the view nears the edge of its
/// layer around `value`; paint a band there and call `set_layer`.
pub const SCROLL: u16 = 9;
pub const BAND: u16 = 10;
/// On touch events: `code` is SCROLLING when the compositor took the gesture
/// as a scroll, so the app should not treat it as a tap.
pub const SCROLLING: u16 = 1;

/// How far a finger moves before a press becomes a drag.
const SLOP: i32 = 12;
/// Fling friction: velocity falls by e every TAU seconds.
const TAU: f32 = 0.325;
const STOP_SPEED: f32 = 30.0;

struct View {
    viewport: Rect,
    layer: Option<Canvas>,
    /// Content y of the layer's first row, and the most the view scrolls.
    origin: i32,
    max_scroll: i32,
    scroll: f32,
    velocity: f32,
    /// A press in the view: start x, y, scroll then, and whether it is a drag.
    press: Option<(i32, i32, f32, bool)>,
    last_move: Option<(i32, Instant)>,
    shown: Option<i32>,
    /// The scroll a band was last asked for (so it is asked once).
    band_asked: Option<i32>,
    indicator: Option<Color>,
    settled: bool,
}

impl View {
    fn moving(&self) -> bool {
        self.velocity != 0.0 && self.press.is_none()
    }
    /// Motion has stopped and the app has not been told where.
    fn settling(&self) -> bool {
        !self.settled && self.press.is_none() && self.velocity == 0.0
    }
    fn height(&self) -> i32 {
        self.viewport.height()
    }
    fn clamp(&mut self) {
        let lo = 0f32;
        let hi = self.max_scroll.max(0) as f32;
        if self.scroll < lo || self.scroll > hi {
            self.scroll = self.scroll.clamp(lo, hi);
            self.velocity = 0.0;
        }
        // Never show rows the layer does not hold.
        if let Some(l) = &self.layer {
            let (top, bottom) = (self.origin as f32, (self.origin + l.h as i32 - self.height()) as f32);
            if bottom >= top && (self.scroll < top || self.scroll > bottom) {
                self.scroll = self.scroll.clamp(top, bottom);
                self.velocity = 0.0;
            }
        }
    }
    /// Whether most of the layer's spare rows are behind the viewport.
    fn near_band_edge(&self) -> bool {
        let Some(l) = &self.layer else { return false };
        let s = self.scroll as i32;
        let (top, bottom) = (self.origin, self.origin + l.h as i32);
        let margin = ((l.h as i32 - self.height()) / 3).clamp(1, self.height().max(1));
        (s - top < margin && top > 0) || (bottom - (s + self.height()) < margin && bottom < self.max_scroll + self.height())
    }
}

struct Shared {
    views: Vec<View>,
    /// Screen areas the app changed through `chrome`, to present.
    damage: Rect,
    /// Events to send the app (settled scrolls, band requests).
    out: Vec<Event>,
    quit: bool,
}

pub struct Compositor {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    chrome: Mutex<Canvas>,
}

impl Compositor {
    /// Take over the screen and the event stream: the compositor thread
    /// presents, and the returned stream carries the app's events (touches
    /// marked SCROLLING when the compositor scrolled with them) plus SCROLL
    /// and BAND.
    pub fn start(mut screen: Screen) -> std::io::Result<(Arc<Compositor>, Events)> {
        let chrome = screen.canvas();
        let shared = Arc::new((
            Mutex::new(Shared { views: Vec::new(), damage: Rect::new(0, 0, 0, 0), out: Vec::new(), quit: false }),
            Condvar::new(),
        ));
        let (tx, rx) = std::sync::mpsc::channel();
        let raw = crate::device::events()?;
        // Input: the compositor sees every event before the app does.
        {
            let shared = shared.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                for mut e in raw.iter() {
                    {
                        let (lock, cond) = &*shared;
                        let mut s = lock.lock().unwrap();
                        input(&mut s, &mut e);
                        cond.notify_one();
                    }
                    if tx.send(e).is_err() {
                        return;
                    }
                }
            });
        }
        // Frames.
        {
            let shared = shared.clone();
            std::thread::spawn(move || compose(screen, shared, tx));
        }
        Ok((Arc::new(Compositor { shared, chrome: Mutex::new(chrome) }), rx))
    }

    /// A scrolling view over `viewport` (screen coordinates). Returns its id.
    pub fn add_view(&self, viewport: Rect) -> usize {
        let mut s = self.shared.0.lock().unwrap();
        s.views.push(View {
            viewport,
            layer: None,
            origin: 0,
            max_scroll: 0,
            scroll: 0.0,
            velocity: 0.0,
            press: None,
            last_move: None,
            shown: None,
            band_asked: None,
            indicator: None,
            settled: true,
        });
        s.views.len() - 1
    }

    /// Move or resize a view (a keyboard covering part of it, say).
    pub fn set_viewport(&self, view: usize, viewport: Rect) {
        self.update(view, |v| {
            v.viewport = viewport;
            v.shown = None;
        });
    }

    /// Let the app reuse a layer's backing store. The last frame remains
    /// visible until the replacement is presented.
    pub fn take_layer(&self, view: usize) -> Option<Canvas> {
        self.shared.0.lock().unwrap().views[view].layer.take()
    }

    /// Give the view a painted layer as wide as its viewport, holding content
    /// rows from `origin`, and the most it may scroll. The layer is painted by
    /// the app on its own time; swapping it in is instant.
    pub fn set_layer(&self, view: usize, layer: Canvas, origin: i32, max_scroll: i32) {
        self.update(view, |v| {
            v.layer = Some(layer);
            v.origin = origin;
            v.max_scroll = max_scroll;
            v.band_asked = None;
            v.shown = None;
            v.clamp();
        });
    }

    /// Draw a thin scroll indicator in this colour at the view's right edge.
    pub fn set_indicator(&self, view: usize, color: Option<Color>) {
        self.update(view, |v| {
            v.indicator = color;
            v.shown = None;
        });
    }

    /// Where the view is scrolled to (content y at its top).
    pub fn scroll(&self, view: usize) -> i32 {
        self.shared.0.lock().unwrap().views[view].scroll.round() as i32
    }

    /// Jump to a scroll position (stops any fling).
    pub fn set_scroll(&self, view: usize, y: i32) {
        self.update(view, |v| {
            v.scroll = y as f32;
            v.velocity = 0.0;
            v.clamp();
            v.shown = None;
        });
    }

    /// Draw on the screen outside the views; `paint` returns the area it
    /// changed, which is presented on the next frame.
    pub fn chrome(&self, paint: impl FnOnce(&mut Canvas) -> Rect) {
        let mut c = self.chrome.lock().unwrap();
        // Keep chrome out of the views, whose pixels the compositor owns.
        let r = paint(&mut c);
        let (lock, cond) = &*self.shared;
        let mut s = lock.lock().unwrap();
        s.damage = s.damage.union(r.intersect(Rect::new(0, 0, W as i32, H as i32)));
        cond.notify_one();
    }

    fn update(&self, view: usize, f: impl FnOnce(&mut View)) {
        let (lock, cond) = &*self.shared;
        let mut s = lock.lock().unwrap();
        if let Some(v) = s.views.get_mut(view) {
            f(v);
        }
        cond.notify_one();
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        let (lock, cond) = &*self.shared;
        lock.lock().unwrap().quit = true;
        cond.notify_one();
    }
}

/// A touch, seen by the compositor before the app: drags inside a view
/// scroll it at once, and their events are marked SCROLLING.
fn input(s: &mut Shared, e: &mut Event) {
    let now = Instant::now();
    for v in s.views.iter_mut() {
        match e.kind {
            TOUCH_DOWN if v.viewport.contains(e.x, e.y) => {
                // Touching a fling catches it.
                v.velocity = 0.0;
                v.press = Some((e.x, e.y, v.scroll, false));
                v.last_move = Some((e.y, now));
            }
            TOUCH_MOVE => {
                let Some((x0, y0, s0, dragging)) = v.press.as_mut() else { continue };
                if !*dragging && ((e.y - *y0).abs() > SLOP || (e.x - *x0).abs() > SLOP) {
                    *dragging = true;
                }
                if *dragging {
                    v.scroll = *s0 - (e.y - *y0) as f32;
                    v.clamp();
                    if let Some((y, t)) = v.last_move {
                        let dt = now.duration_since(t).as_secs_f32();
                        if dt > 0.0 {
                            // Finger velocity, smoothed; the content moves the other way.
                            let speed = -((e.y - y) as f32) / dt;
                            v.velocity = if v.velocity == 0.0 { speed } else { 0.6 * speed + 0.4 * v.velocity };
                        }
                    }
                    v.last_move = Some((e.y, now));
                    v.settled = false;
                    e.code = SCROLLING;
                }
            }
            TOUCH_UP => {
                let Some((_, _, _, dragging)) = v.press.take() else { continue };
                if dragging {
                    // A finger that stopped before lifting does not fling.
                    if v.last_move.map_or(true, |(_, t)| now.duration_since(t).as_millis() > 80) {
                        v.velocity = 0.0;
                    }
                    if v.velocity.abs() < STOP_SPEED {
                        v.velocity = 0.0;
                    }
                    v.settled = false;
                    e.code = SCROLLING;
                } else {
                    v.velocity = 0.0;
                }
            }
            _ => {}
        }
    }
}

/// The compositor thread: wait for work, step the flings once a frame, copy
/// what moved to the screen, present.
fn compose(mut screen: Screen, shared: Arc<(Mutex<Shared>, Condvar)>, tx: std::sync::mpsc::Sender<Event>) {
    let mut c = screen.canvas();
    let mut last = Instant::now();
    loop {
        let animating = {
            let (lock, cond) = &*shared;
            let mut s = lock.lock().unwrap();
            loop {
                if s.quit {
                    return;
                }
                let work = !s.damage.is_empty()
                    || !s.out.is_empty()
                    || s.views.iter().any(|v| v.moving() || v.shown != Some(v.scroll.round() as i32) || v.settling());
                if work {
                    break;
                }
                s = cond.wait(s).unwrap();
            }
            s.views.iter().any(|v| v.moving())
        };
        // Motion follows the display; a single change goes out at once.
        if animating {
            screen.wait_vsync();
        }
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f32().min(0.05);
        last = now;
        let (damage, out) = step(&mut shared.0.lock().unwrap(), &mut c, dt);
        if !damage.is_empty() {
            screen.show(&c, damage);
        }
        for e in out {
            if tx.send(e).is_err() {
                return;
            }
        }
    }
}

/// One frame: advance flings by dt, copy views that moved onto `c`, and
/// collect the area to present and the events to send.
fn step(s: &mut Shared, c: &mut Canvas, dt: f32) -> (Rect, Vec<Event>) {
    let mut damage = std::mem::replace(&mut s.damage, Rect::new(0, 0, 0, 0));
    let mut out = std::mem::take(&mut s.out);
    for (id, v) in s.views.iter_mut().enumerate() {
        if v.moving() {
            v.scroll += v.velocity * dt;
            v.velocity *= (-dt / TAU).exp();
            if v.velocity.abs() < STOP_SPEED {
                v.velocity = 0.0;
            }
            v.clamp();
        }
        let y = v.scroll.round() as i32;
        if v.shown != Some(y) {
            blit(c, v);
            v.shown = Some(y);
            damage = damage.union(v.viewport);
        }
        if v.near_band_edge() && v.band_asked.map_or(true, |a| (a - y).abs() > v.height() / 2) {
            v.band_asked = Some(y);
            out.push(Event { kind: BAND, code: id as u16, x: 0, y: 0, value: y });
        }
        if v.settling() {
            v.settled = true;
            out.push(Event { kind: SCROLL, code: id as u16, x: 0, y: 0, value: y });
        }
    }
    (damage, out)
}

/// The view's visible window of its layer, onto the screen canvas.
fn blit(c: &mut Canvas, v: &View) {
    let r = v.viewport.intersect(Rect::new(0, 0, W as i32, H as i32));
    let Some(layer) = &v.layer else { return };
    let top = v.scroll.round() as i32 - v.origin;
    let w = (r.width() as usize).min(layer.w);
    for row in 0..r.height() {
        let src_y = top + row;
        let dst = (r.y0 + row) as usize * W + r.x0 as usize;
        if src_y < 0 || src_y >= layer.h as i32 {
            continue;
        }
        let src = src_y as usize * layer.w;
        c.px[dst..dst + w].copy_from_slice(&layer.px[src..src + w]);
    }
    if let (Some(color), true) = (v.indicator, v.max_scroll > 0) {
        let track = r.height() - 20;
        let content = v.max_scroll + r.height();
        let thumb = (track * r.height() / content.max(1)).max(40);
        let at = r.y0 + 10 + (track - thumb) * (v.scroll.round() as i32).clamp(0, v.max_scroll) / v.max_scroll.max(1);
        c.round_rect(Rect::new(r.x1 - 10, at, r.x1 - 4, at + thumb), 3.0, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(viewport: Rect, layer_h: usize, origin: i32, max_scroll: i32) -> View {
        let mut layer = Canvas::new(viewport.width() as usize, layer_h);
        for y in 0..layer_h {
            // Row y of the layer holds content row origin + y, as a colour.
            layer.fill(Rect::new(0, y as i32, viewport.width(), y as i32 + 1), 0xff00_0000 | (origin as u32 + y as u32));
        }
        View {
            viewport, layer: Some(layer), origin, max_scroll, scroll: 0.0, velocity: 0.0, press: None, last_move: None,
            shown: None, band_asked: None, indicator: None, settled: true,
        }
    }
    fn shared(v: View) -> Shared {
        Shared { views: vec![v], damage: Rect::new(0, 0, 0, 0), out: Vec::new(), quit: false }
    }
    fn touch(kind: u16, y: i32) -> Event {
        Event { kind, code: 0, x: 100, y, value: 0 }
    }

    #[test]
    fn drag_scrolls_with_the_finger_and_shows_the_right_rows() {
        let mut s = shared(view(Rect::new(0, 100, 720, 1600), 6000, 0, 5000));
        let mut c = Canvas::new(W, H);
        let mut e = touch(TOUCH_DOWN, 1000);
        input(&mut s, &mut e);
        assert_eq!(e.code, 0);
        // Within the slop the press may still be a tap; past it, a drag.
        for (y, code) in [(990, 0), (900, SCROLLING), (700, SCROLLING)] {
            let mut e = touch(TOUCH_MOVE, y);
            input(&mut s, &mut e);
            assert_eq!(e.code, code);
        }
        assert_eq!(s.views[0].scroll as i32, 300);
        let (damage, _) = step(&mut s, &mut c, 0.016);
        assert_eq!(damage, Rect::new(0, 100, 720, 1600));
        // The view's top row shows content row 300.
        assert_eq!(c.get(5, 100), 0xff00_0000 | 300);
        assert_eq!(c.get(5, 1599), 0xff00_0000 | (300 + 1499));
    }

    #[test]
    fn a_tap_is_not_a_scroll() {
        let mut s = shared(view(Rect::new(0, 100, 720, 1600), 6000, 0, 5000));
        for (kind, y) in [(TOUCH_DOWN, 500), (TOUCH_MOVE, 505), (TOUCH_UP, 505)] {
            let mut e = touch(kind, y);
            input(&mut s, &mut e);
            assert_eq!(e.code, 0);
        }
        assert_eq!(s.views[0].scroll, 0.0);
    }

    #[test]
    fn a_flick_flings_then_settles_once() {
        let mut s = shared(view(Rect::new(0, 100, 720, 1600), 20000, 0, 19000));
        let mut c = Canvas::new(W, H);
        let mut e = touch(TOUCH_DOWN, 1200);
        input(&mut s, &mut e);
        // Moves 16 ms apart at about 3000 px/s.
        for y in [1150, 1100, 1050, 1000] {
            std::thread::sleep(std::time::Duration::from_millis(16));
            let mut e = touch(TOUCH_MOVE, y);
            input(&mut s, &mut e);
        }
        let mut e = touch(TOUCH_UP, 1000);
        input(&mut s, &mut e);
        let released = s.views[0].scroll;
        assert!(s.views[0].velocity > 1000.0, "velocity {}", s.views[0].velocity);
        let mut settled = Vec::new();
        for _ in 0..600 {
            let (_, out) = step(&mut s, &mut c, 1.0 / 60.0);
            settled.extend(out.into_iter().filter(|e| e.kind == SCROLL));
        }
        let end = s.views[0].scroll;
        assert!(end > released + 300.0, "fling went {released} -> {end}");
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].value, end.round() as i32);
    }

    #[test]
    fn a_centered_short_band_does_not_request_itself_again() {
        let mut v = view(Rect::new(0, 100, 720, 1600), 3000, 2000, 7000);
        v.scroll = 2750.0;
        assert!(!v.near_band_edge());
        v.scroll = 3470.0;
        assert!(v.near_band_edge());
    }

    #[test]
    fn scrolling_stops_at_the_ends_and_asks_for_the_next_band() {
        let mut s = shared(view(Rect::new(0, 100, 720, 1600), 4500, 0, 10000));
        let mut c = Canvas::new(W, H);
        s.views[0].scroll = 5000.0; // past the band
        s.views[0].clamp();
        assert_eq!(s.views[0].scroll as i32, 4500 - 1500);
        let (_, out) = step(&mut s, &mut c, 0.016);
        assert!(out.iter().any(|e| e.kind == BAND && e.value == 3000));
        s.views[0].scroll = -50.0;
        s.views[0].clamp();
        assert_eq!(s.views[0].scroll, 0.0);
    }
}
