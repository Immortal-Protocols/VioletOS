//! Ladybird for VioletOS: a web browser built on Ladybird's HTML tokenizer
//! and tree builder (LibWeb's Rust HTML parser), running as an open app.
//!
//! Ladybird parses each page into the Rust DOM in dom.rs; style.rs computes
//! styles from the HTML defaults, `<style>` sheets and `style` attributes;
//! layout.rs lays the page out for the 720-pixel-wide screen; this file
//! paints it, scrolls it, follows links and keeps the history. Scripts are
//! not run. Pages load over HTTPS (rustls) or HTTP in a background thread.
mod ak_runtime;
mod dom;
mod layout;
mod net;
mod style;
mod url;
mod names {
    #![allow(non_upper_case_globals)]
    include!(concat!(env!("OUT_DIR"), "/names.rs"));
}

use std::sync::mpsc;
use std::time::{Duration, Instant};

use layout::{Item, Layout};
use url::Url;
use violet::device::{TOUCH_DOWN, TOUCH_MOVE, TOUCH_UP};
use violet::{colors, mix, rgb, Canvas, Fonts, Rect, Screen, H, W};

const TOOLBAR: i32 = 124;
const BACK_BOX: Rect = Rect::new(200, 34, 284, 92);
const ADDRESS_BOX: Rect = Rect::new(298, 34, 694, 92);
const PROGRESS: Rect = Rect::new(0, TOOLBAR - 6, W as i32, TOOLBAR);
const KEY_H: i32 = 96;
const KEYBOARD_TOP: i32 = H as i32 - 5 * KEY_H - 16;
const KEY_ROWS: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl:", "zxcvbnm.-/"];
const DRAG_SLOP: i32 = 18;

const START_PAGE: &str = r#"<!doctype html>
<html><head><title>Ladybird</title>
<style>
body { background: #fbf8ff; color: #241b33; }
h1 { color: #6b2fa0; margin-bottom: 4px; }
.lead { color: #5a5068; }
h2 { font-size: 1.1em; color: #6b2fa0; margin-top: 1.4em; }
li { margin-bottom: 10px; }
a { color: #1a55c4; }
.note { font-size: 0.85em; color: #7a7088; }
</style></head>
<body>
<h1>Ladybird</h1>
<p class=lead>Ladybird's HTML parser, running on VioletOS. Tap the address bar to go anywhere.</p>
<h2>Try</h2>
<ul>
<li><a href="http://info.cern.ch/hypertext/WWW/TheProject.html">The first web page</a> (HTTP)</li>
<li><a href="https://example.com/">example.com</a> (HTTPS)</li>
<li><a href="https://news.ycombinator.com/">Hacker News</a></li>
<li><a href="https://lite.cnn.com/">CNN Lite</a></li>
<li><a href="https://text.npr.org/">NPR Text</a></li>
<li><a href="https://en.m.wikipedia.org/wiki/Ladybird_(web_browser)">Ladybird on Wikipedia</a></li>
<li><a href="https://ladybird.org/">ladybird.org</a></li>
</ul>
<p class=note>Pages are parsed by Ladybird's tokenizer and tree builder from LibWeb, compiled for ARMv7 and linked into this Rust app. Scripts do not run.</p>
</body></html>"#;

#[derive(Clone, Debug, PartialEq)]
enum Location {
    Start,
    Web(Url),
}

impl Location {
    fn text(&self) -> String {
        match self {
            Location::Start => "about:home".into(),
            Location::Web(u) => u.document(),
        }
    }
}

struct Shown {
    location: Location,
    layout: Layout,
}

/// A page fetched and parsed, with its linked style sheets, ready for style and layout.
struct Fetched {
    page: net::Page,
    document: dom::Document,
    sheets: Vec<String>,
    parse_ms: u128,
}

type Loaded = (u64, Location, Result<Fetched, String>);

const MAX_SHEETS: usize = 6;
const MAX_SHEET_BYTES: usize = 512 << 10;

/// Parse a fetched page with Ladybird's parser and, if asked, fetch the style
/// sheets it links to. Runs on the loader thread.
fn prepare(name: &str, page: net::Page, fetch_sheets: bool) -> Fetched {
    let started = Instant::now();
    let text = net::decode(&page.body, &page.content_type);
    let html = if page.content_type.contains("html") || page.content_type.contains("xml") || text.trim_start().starts_with('<') {
        text
    } else if page.content_type.starts_with("text/") {
        format!("<pre>{}</pre>", escape(&text))
    } else {
        format!("<h1>{}</h1><p>{} bytes of {}.</p>", escape(name), page.body.len(), escape(&page.content_type))
    };
    let document = dom::parse(&html);
    let parse_ms = started.elapsed().as_millis();
    let mut sheets = Vec::new();
    if fetch_sheets {
        let links: Vec<String> = (1..=document.nodes.len())
            .filter(|&n| document.name(n) == "link")
            .filter(|&n| document.attribute(n, "rel").is_some_and(|r| r.split_whitespace().any(|w| w.eq_ignore_ascii_case("stylesheet"))))
            .filter(|&n| document.attribute(n, "media").is_none_or(style::media_applies))
            .filter_map(|n| document.attribute(n, "href").map(str::to_string))
            .take(MAX_SHEETS)
            .collect();
        for href in links {
            let Some(url) = page.url.join(&href) else { continue };
            match net::load(&url) {
                Ok(css) if css.status == 200 && css.body.len() <= MAX_SHEET_BYTES => {
                    println!("ladybird: style sheet {} ({} bytes)", url.document(), css.body.len());
                    sheets.push(String::from_utf8_lossy(&css.body).into_owned());
                }
                Ok(css) => println!("ladybird: style sheet {} skipped (status {}, {} bytes)", url.document(), css.status, css.body.len()),
                Err(e) => println!("ladybird: style sheet {} failed: {e}", url.document()),
            }
        }
    }
    Fetched { page, document, sheets, parse_ms }
}

struct Browser {
    fonts: Fonts,
    c: Canvas,
    page: Option<Shown>,
    scroll: i32,
    history: Vec<(Location, i32)>,
    loading: Option<(u64, Location)>,
    generation: u64,
    status: String,
    editing: Option<String>,
    tx: mpsc::Sender<Loaded>,
}

fn viewport() -> i32 {
    H as i32 - TOOLBAR
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// What the address bar's text means: a URL, a host name, or a search.
fn interpret(text: &str) -> Option<Url> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(u) = Url::parse(t) {
        return Some(u);
    }
    if !t.contains(' ') && (t.contains('.') || t.contains(':') || t.starts_with("localhost")) {
        return Url::parse(&format!("https://{t}"));
    }
    let query: String = t.split_whitespace().collect::<Vec<_>>().join("+");
    Url::parse(&format!("https://lite.duckduckgo.com/lite/?q={query}"))
}

impl Browser {
    fn navigate(&mut self, to: Location, push: bool) {
        if push && let Some(page) = &self.page {
            self.history.push((page.location.clone(), self.scroll));
        }
        self.generation += 1;
        println!("ladybird: navigate {}", to.text());
        match &to {
            Location::Start => {
                let page = net::Page { url: Url::parse("http://start/").unwrap(), status: 200, content_type: "text/html".into(), body: START_PAGE.as_bytes().to_vec() };
                self.show(Location::Start, prepare("about:home", page, false));
            }
            Location::Web(url) => {
                self.loading = Some((self.generation, to.clone()));
                self.status = format!("Loading {}…", url.host());
                let (tx, generation, url, location) = (self.tx.clone(), self.generation, url.clone(), to.clone());
                std::thread::spawn(move || {
                    let result = net::load(&url).map(|page| prepare(&url.document(), page, true)).map_err(|e| e.to_string());
                    let _ = tx.send((generation, location, result));
                });
            }
        }
    }

    fn show(&mut self, location: Location, fetched: Fetched) {
        let started = Instant::now();
        let Fetched { page, document, sheets, parse_ms } = fetched;
        let sheet = style::Sheet::from_document(&document, &sheets);
        let styles = style::compute(&document, &sheet);
        let layout = layout::layout(&document, &styles, &self.fonts, W as i32);
        let title = document.title();
        let shown_as = match &location {
            Location::Web(_) => page.url.document(),
            Location::Start => location.text(),
        };
        println!(
            "ladybird: loaded {} status={} elements={} rules={} links={} height={} title={:?} parse={}ms layout={}ms",
            shown_as,
            page.status,
            document.elements(),
            sheet.rule_count(),
            layout.links.len(),
            layout.height,
            title,
            parse_ms,
            started.elapsed().as_millis()
        );
        let location = match location {
            Location::Web(_) => Location::Web(page.url.clone()),
            start => start,
        };
        self.scroll = match &page.url.fragment {
            Some(f) => layout.anchors.get(f).copied().unwrap_or(0),
            None => 0,
        };
        self.page = Some(Shown { location, layout });
        self.status.clear();
        self.clamp_scroll();
    }

    fn loaded(&mut self, (generation, location, result): Loaded) {
        if self.loading.as_ref().is_none_or(|(g, _)| *g != generation) {
            return;
        }
        self.loading = None;
        match result {
            Ok(fetched) => self.show(location, fetched),
            Err(e) => {
                println!("ladybird: failed {} {e}", location.text());
                let html = format!(
                    "<title>Could not load</title><h1>Could not load this page</h1><p>{}</p><p style=\"color:#777\">{}</p>",
                    escape(&location.text()),
                    escape(&e)
                );
                let url = match &location {
                    Location::Web(u) => u.clone(),
                    Location::Start => Url::parse("http://start/").unwrap(),
                };
                let name = location.text();
                self.show(location, prepare(&name, net::Page { url, status: 0, content_type: "text/html".into(), body: html.into_bytes() }, false));
            }
        }
    }

    fn back(&mut self) {
        if let Some((location, scroll)) = self.history.pop() {
            if let Location::Web(u) = &location {
                println!("ladybird: back to {}", u.document());
            }
            let same = self.page.as_ref().is_some_and(|p| match (&p.location, &location) {
                (Location::Web(a), Location::Web(b)) => a.document() == b.document(),
                (a, b) => a == b,
            });
            if same {
                self.scroll = scroll;
                self.clamp_scroll();
                return;
            }
            self.navigate(location, false);
            if self.loading.is_none() {
                self.scroll = scroll;
                self.clamp_scroll();
            }
        }
    }

    fn clamp_scroll(&mut self) {
        let height = self.page.as_ref().map_or(0, |p| p.layout.height);
        let room = if self.editing.is_some() { KEYBOARD_TOP - TOOLBAR } else { viewport() };
        self.scroll = self.scroll.clamp(0, (height - room + 40).max(0));
    }

    fn follow(&mut self, href: &str) {
        let base = match self.page.as_ref().map(|p| &p.location) {
            Some(Location::Web(u)) => u.clone(),
            _ => Url::parse("http://start/").unwrap(),
        };
        let Some(target) = base.join(href) else {
            self.status = "Only http and https links open here".into();
            return;
        };
        println!("ladybird: link {href} -> {}", target.document());
        if target.document() == base.document() && target.fragment.is_some() {
            let y = self.page.as_ref().and_then(|p| p.layout.anchors.get(target.fragment.as_ref().unwrap()).copied());
            if let Some(y) = y {
                self.history.push((self.page.as_ref().unwrap().location.clone(), self.scroll));
                self.scroll = y;
                self.clamp_scroll();
                return;
            }
        }
        self.navigate(Location::Web(target), true);
    }

    // Drawing.

    fn draw_page(&mut self) {
        let Some(page) = &self.page else {
            self.c.fill(Rect::new(0, TOOLBAR, W as i32, H as i32), rgb(0xff, 0xff, 0xff));
            return;
        };
        let l = &page.layout;
        self.c.fill(Rect::new(0, TOOLBAR, W as i32, H as i32), l.background);
        let dy = TOOLBAR - self.scroll;
        let (top, bottom) = (self.scroll - 200, self.scroll + viewport() + 200);
        for item in &l.items {
            match item {
                Item::Fill { r, color } => {
                    if r.y1 >= top && r.y0 <= bottom {
                        self.c.fill(r.offset(0, dy).intersect(Rect::new(0, TOOLBAR, W as i32, H as i32)), *color);
                    }
                }
                Item::Text { x, baseline, text, size, bold, color } => {
                    if *baseline >= top && *baseline <= bottom {
                        let face = if *bold { self.fonts.bold(*size) } else { self.fonts.regular(*size) };
                        face.draw(&mut self.c, text, *x, baseline + dy, *color);
                    }
                }
                Item::Dot { cx, cy, radius, color } => {
                    if *cy >= top && *cy <= bottom {
                        self.c.circle(*cx as f32, (cy + dy) as f32, *radius, *color);
                    }
                }
            }
        }
        // A scroll indicator when the page is taller than the screen.
        if l.height > viewport() {
            let track = viewport() - 20;
            let thumb = (track * viewport() / l.height).max(40);
            let at = TOOLBAR + 10 + (track - thumb) * self.scroll / (l.height - viewport() + 40).max(1);
            self.c.round_rect(Rect::new(W as i32 - 10, at, W as i32 - 4, at + thumb), 3.0, mix(l.background, rgb(0, 0, 0), 0.35));
        }
    }

    fn draw_toolbar(&mut self) {
        let c = &mut self.c;
        c.fill(Rect::new(0, 0, W as i32, TOOLBAR), colors::BACKGROUND);
        let medium = self.fonts.medium(24.0);
        violet::button(c, &medium, violet::HOME_BOX, "‹ Home", colors::PANEL, colors::INK);
        let back_ink = if self.history.is_empty() { colors::MUTED } else { colors::INK };
        violet::button(c, &self.fonts.bold(34.0), BACK_BOX, "‹", colors::PANEL, back_ink);
        let editing = self.editing.is_some();
        c.round_rect(ADDRESS_BOX, 22.0, if editing { colors::INK } else { colors::PANEL });
        let body = self.fonts.regular(24.0);
        let base = ADDRESS_BOX.y0 + (ADDRESS_BOX.height() + body.ascent() - body.descent()) / 2;
        let room = ADDRESS_BOX.width() - 40;
        if let Some(text) = &self.editing {
            // Show the end of what is being typed, and a caret.
            let mut shown: Vec<char> = text.chars().collect();
            while body.width(&shown.iter().collect::<String>()) > room - 10 && !shown.is_empty() {
                shown.remove(0);
            }
            let shown: String = shown.into_iter().collect();
            let end = body.draw(c, &shown, ADDRESS_BOX.x0 + 20, base, colors::BACKGROUND);
            c.fill(Rect::new(end + 2, base - body.ascent(), end + 5, base - body.descent()), colors::ACCENT);
        } else {
            let label = if !self.status.is_empty() {
                self.status.clone()
            } else {
                match self.page.as_ref().map(|p| &p.location) {
                    Some(Location::Web(u)) => format!("{}{}", u.authority, if u.path == "/" { "" } else { &u.path }),
                    _ => "Search or enter address".into(),
                }
            };
            let ink = if self.status.is_empty() && self.page.as_ref().is_some_and(|p| p.location != Location::Start) { colors::INK } else { colors::MUTED };
            body.draw(c, &body.fit(&label, room), ADDRESS_BOX.x0 + 20, base, ink);
        }
        if self.loading.is_some() {
            c.fill(PROGRESS, colors::PANEL);
            let t = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() % 1200) as i32;
            let x = t * (W as i32 + 240) / 1200 - 240;
            c.fill(Rect::new(x, PROGRESS.y0, x + 240, PROGRESS.y1), colors::ACCENT);
        }
    }

    fn key_at(x: i32, y: i32) -> Option<&'static str> {
        if y < KEYBOARD_TOP + 8 {
            return None;
        }
        let row = (y - KEYBOARD_TOP - 8) / KEY_H;
        let col = (x / 72).clamp(0, 9) as usize;
        match row {
            0..=3 => Some(&KEY_ROWS[row as usize][col..col + 1]),
            4 => Some(match x {
                0..144 => ".com",
                144..216 => "_",
                216..432 => " ",
                432..576 => "⌫",
                _ => "Go",
            }),
            _ => None,
        }
    }

    fn draw_keyboard(&mut self) {
        if self.editing.is_none() {
            return;
        }
        let c = &mut self.c;
        c.fill(Rect::new(0, KEYBOARD_TOP, W as i32, H as i32), colors::BACKGROUND);
        let face = self.fonts.medium(34.0);
        let key = |c: &mut Canvas, r: Rect, label: &str, bg| {
            c.round_rect(r.inset(4), 14.0, bg);
            let base = r.y0 + (r.height() + face.ascent() - face.descent()) / 2;
            face.centered(c, label, r.x0, r.x1, base, if bg == colors::ACCENT { colors::BACKGROUND } else { colors::INK });
        };
        for (row, keys) in KEY_ROWS.iter().enumerate() {
            for (col, k) in keys.chars().enumerate() {
                let r = Rect::new(col as i32 * 72, KEYBOARD_TOP + 8 + row as i32 * KEY_H, col as i32 * 72 + 72, KEYBOARD_TOP + 8 + (row as i32 + 1) * KEY_H);
                key(c, r, &k.to_string(), colors::PANEL);
            }
        }
        let y0 = KEYBOARD_TOP + 8 + 4 * KEY_H;
        for (x0, x1, label, bg) in [(0, 144, ".com", colors::PANEL), (144, 216, "_", colors::PANEL), (216, 432, "space", colors::PANEL), (432, 576, "⌫", colors::PANEL), (576, 720, "Go", colors::ACCENT)] {
            key(c, Rect::new(x0, y0, x1, y0 + KEY_H), label, bg);
        }
    }

    fn draw(&mut self) {
        self.draw_page();
        self.draw_toolbar();
        self.draw_keyboard();
    }

    // Input.

    fn tap(&mut self, x: i32, y: i32) {
        if violet::HOME_BOX.contains(x, y) {
            violet::home();
            return;
        }
        if BACK_BOX.contains(x, y) {
            self.editing = None;
            self.back();
            return;
        }
        if ADDRESS_BOX.contains(x, y) {
            if self.editing.is_none() {
                self.editing = Some(String::new());
                println!("ladybird: editing address");
            }
            return;
        }
        if let Some(text) = self.editing.as_mut() {
            if let Some(k) = Self::key_at(x, y) {
                match k {
                    "⌫" => {
                        text.pop();
                    }
                    "Go" => {
                        let typed = std::mem::take(text);
                        self.editing = None;
                        println!("ladybird: go {typed:?}");
                        if let Some(u) = interpret(&typed) {
                            self.navigate(Location::Web(u), true);
                        }
                    }
                    k => text.push_str(k),
                }
                return;
            }
            self.editing = None;
            return;
        }
        if y < TOOLBAR {
            return;
        }
        let (px, py) = (x, y - TOOLBAR + self.scroll);
        let href = self.page.as_ref().and_then(|p| {
            // The closest link area under the finger, with a little slack.
            p.layout.links.iter().filter(|(r, _)| r.inset(-8).contains(px, py)).min_by_key(|(r, _)| {
                let (cx, cy) = ((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2);
                (cx - px).abs() / 4 + (cy - py).abs()
            })
        });
        if let Some((_, href)) = href {
            let href = href.clone();
            self.follow(&href);
        }
    }
}

/// Fill in the tag and attribute names the tree builder compares against.
pub fn register_names() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| names::register(&mut ak_runtime::intern_str));
}

/// On the Mac only: lay out an HTML file and write what the screen would show
/// as a PPM image, for checking layout without booting QEMU.
#[cfg(target_os = "macos")]
fn render_to_file(input: &str, output: &str, scroll: i32) {
    let (tx, _rx) = mpsc::channel::<Loaded>();
    let mut b = Browser { fonts: Fonts::load(), c: Canvas::new(W, H), page: None, scroll: 0, history: Vec::new(), loading: None, generation: 0, status: String::new(), editing: None, tx };
    let body = std::fs::read(input).expect("input");
    let url = Url::parse("http://localhost/").unwrap();
    let sheets: Vec<String> = std::env::args().skip(5).map(|p| std::fs::read_to_string(p).expect("style sheet")).collect();
    let mut fetched = prepare(input, net::Page { url: url.clone(), status: 200, content_type: "text/html".into(), body }, false);
    fetched.sheets = sheets;
    b.show(Location::Web(url), fetched);
    b.scroll = scroll;
    b.clamp_scroll();
    b.draw();
    let mut ppm = format!("P6 {W} {H} 255\n").into_bytes();
    for p in &b.c.px {
        ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
    }
    std::fs::write(output, ppm).expect("output");
}

fn main() {
    #[cfg(target_os = "macos")]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.len() >= 4 && args[1] == "--render" {
            render_to_file(&args[2], &args[3], args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0));
            return;
        }
    }
    let mut screen = Screen::open().expect("no screen");
    let events = violet::events().expect("no events");
    let (tx, results) = mpsc::channel::<Loaded>();
    let mut b = Browser {
        fonts: Fonts::load(),
        c: Canvas::new(W, H),
        page: None,
        scroll: 0,
        history: Vec::new(),
        loading: None,
        generation: 0,
        status: String::new(),
        editing: None,
        tx,
    };
    println!("ladybird: starting (Ladybird HTML parser {})", env!("LADYBIRD_COMMIT"));
    b.navigate(Location::Start, false);
    b.draw();
    screen.show_all(&b.c);

    let mut touch: Option<(i32, i32, i32, bool)> = None; // start x, y, scroll, dragging
    let mut dirty = false;
    let mut last_frame = Instant::now();
    loop {
        let wait = if b.loading.is_some() { 60 } else { 250 };
        match events.recv_timeout(Duration::from_millis(wait)) {
            Ok(e) if e.focus_gained() => dirty = true,
            Ok(e) if e.kind == TOUCH_DOWN => touch = Some((e.x, e.y, b.scroll, false)),
            Ok(e) if e.kind == TOUCH_MOVE => {
                if let Some((x0, y0, s0, dragging)) = touch.as_mut() {
                    if !*dragging && ((e.y - *y0).abs() > DRAG_SLOP || (e.x - *x0).abs() > DRAG_SLOP) && *y0 >= TOOLBAR {
                        *dragging = true;
                    }
                    if *dragging {
                        b.scroll = *s0 - (e.y - *y0);
                        b.clamp_scroll();
                        dirty = true;
                    }
                }
            }
            Ok(e) if e.kind == TOUCH_UP => {
                if let Some((x0, y0, _, dragging)) = touch.take() {
                    if dragging {
                        println!("ladybird: scrolled to {}", b.scroll);
                    } else {
                        b.tap(x0, y0);
                    }
                    dirty = true;
                }
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
        while let Ok(r) = results.try_recv() {
            b.loaded(r);
            dirty = true;
        }
        if b.loading.is_some() {
            dirty = true;
        }
        // While dragging, draw at most every 40 ms and only once the queue is empty.
        if dirty && (touch.is_none() || last_frame.elapsed() >= Duration::from_millis(40)) {
            b.draw();
            screen.show_all(&b.c);
            dirty = false;
            last_frame = Instant::now();
        }
    }
}
