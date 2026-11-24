//! Open files and the /dev/violet devices: the screen (a 720x1600 BGRA8888
//! double buffer an app maps and presents), touch and lifecycle events, and
//! a line-oriented system control file. Also the app list ("shell") the Open
//! Runtime draws when no app is in front.
use super::abi::*;
use super::sys::{self, FRAMEBUFFER, PAGE};
use super::{request, Runtime, Wait, MAX_APPS};
use crate::protocol::*;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

pub const WIDTH: usize = 720;
pub const HEIGHT: usize = 1600;
pub const STRIDE: usize = WIDTH * 4;
pub const BUFFER_BYTES: usize = (STRIDE * HEIGHT + PAGE - 1) & !(PAGE - 1);

pub enum Kind {
    File { path: String },
    Dir { path: String, entries: Vec<(String, bool)>, pos: usize },
    Screen,
    /// 48 kHz s16 PCM to the Audio service; `pending` holds a partial frame.
    Audio { channels: u32, pending: Vec<u8> },
    Events,
    Sys { pending: Vec<u8> },
    Log,
    Null,
    Random,
    /// Interest: (fd, events, data). `reported` holds, for edge-triggered
    /// entries (EPOLLET), the readiness last reported, so only changes wake.
    Epoll { interest: Vec<(i32, u32, u64)>, reported: Vec<(i32, u32)> },
    EventFd { count: u64 },
    Pipe { buffer: Rc<RefCell<VecDeque<u8>>>, write: bool, peer_open: Rc<RefCell<u32>> },
    /// A TCP socket: its slot once connect was called, and the peer.
    Tcp { slot: Option<usize>, peer: (u32, u16) },
    /// A UDP socket. Only DNS (port 53) is served, by the Open Runtime itself.
    Udp { peer: Option<(u32, u16)>, replies: VecDeque<Vec<u8>> },
}

pub struct OpenFile {
    pub kind: Kind,
    pub offset: u64,
    pub flags: u32,
    /// SO_RCVTIMEO in nanoseconds; 0 waits forever.
    pub recv_timeout: u64,
}
impl OpenFile {
    pub fn new(kind: Kind, flags: u32) -> Self {
        Self { kind, offset: 0, flags, recv_timeout: 0 }
    }
}
impl Drop for OpenFile {
    fn drop(&mut self) {
        if let Kind::Pipe { write: true, peer_open, .. } = &self.kind {
            *peer_open.borrow_mut() = 0;
        }
        if let Kind::Tcp { slot: Some(slot), .. } = self.kind {
            super::net::close(slot);
        }
    }
}

pub struct Surface {
    /// Pool pages: buffer 0 then buffer 1.
    pub pages: Vec<u32>,
    pub shown: u32,
}
impl Surface {
    fn byte(&self, offset: usize) -> *mut u8 {
        let page = self.pages[offset / PAGE];
        (super::pool::page_va(page) + offset % PAGE) as *mut u8
    }
}

#[derive(Default)]
pub struct Shell {
    pub dirty: bool,
    /// A touch counts only after the finger has been lifted since coming to the front.
    pub armed: bool,
    held: bool,
    down_at: Option<(i32, i32, u64)>,
    pub message: String,
}

mod font {
    include!(env!("VIOLET_GLYPHS_RS"));
    pub fn tables() -> (&'static [u8], &'static [[u8; 5]]) {
        (ALPHABET, GLYPHS)
    }
}
fn glyph(c: u8) -> Option<&'static [u8; 5]> {
    let (alphabet, glyphs) = font::tables();
    let c = c.to_ascii_uppercase();
    alphabet.iter().position(|&a| a == c).map(|i| &glyphs[i])
}
pub const BG: u32 = 0xff191620;
pub const PANEL: u32 = 0xff302a39;
pub const INK: u32 = 0xffeeeaf8;
pub const ACCENT: u32 = 0xffd1b0fe;

fn fill(x: usize, y: usize, w: usize, h: usize, color: u32) {
    let fb = FRAMEBUFFER as *mut u32;
    for row in y..(y + h).min(HEIGHT) {
        for col in x..(x + w).min(WIDTH) {
            unsafe { fb.add(row * WIDTH + col).write_volatile(color) };
        }
    }
    if w == WIDTH && h == HEIGHT {
        sys::clean_framebuffer(0, HEIGHT); // a screen cleared outside draw_shell
    }
}
fn text(x: usize, y: usize, s: &str, scale: usize, color: u32) {
    for (n, c) in s.bytes().enumerate() {
        let Some(g) = glyph(c) else { continue };
        for (col, &bits) in g.iter().enumerate() {
            for bit in 0..7 {
                if bits & (1 << bit) != 0 {
                    fill(x + (6 * n + col) * scale, y + bit * scale, scale, scale, color);
                }
            }
        }
    }
}

/// The PCM ring shared with Audio (see services/audio/process.rs).
const PCM_HEADER: *mut u32 = 0x0910_0000 as *mut u32;
const PCM_RING: *mut [i16; 2] = 0x0910_1000 as *mut [i16; 2];
const PCM_FRAMES: u32 = 16 * 4096 / 4;
/// Audio writes its magic when it starts; the pages exist only with Audio.
pub fn audio_present(mapped: bool) -> bool {
    mapped && unsafe { PCM_HEADER.read_volatile() } == 0x314d_4350
}
fn pcm_space() -> u32 {
    let (written, played) = unsafe { (PCM_HEADER.add(2).read_volatile(), PCM_HEADER.add(3).read_volatile()) };
    PCM_FRAMES - written.wrapping_sub(played).min(PCM_FRAMES)
}
/// Queue whole frames from `data` (after any partial frame left over). Returns
/// the bytes taken, which may be fewer than given when the ring is full.
pub fn audio_write(channels: u32, pending: &mut Vec<u8>, data: &[u8]) -> usize {
    let frame_bytes = 2 * channels as usize;
    let room = pcm_space() as usize * frame_bytes;
    let take = data.len().min(room.saturating_sub(pending.len()));
    if take == 0 {
        return 0;
    }
    pending.extend_from_slice(&data[..take]);
    let whole = pending.len() / frame_bytes;
    let mut written = unsafe { PCM_HEADER.add(2).read_volatile() };
    for f in 0..whole {
        let at = f * frame_bytes;
        let l = i16::from_le_bytes([pending[at], pending[at + 1]]);
        let r = if channels == 2 { i16::from_le_bytes([pending[at + 2], pending[at + 3]]) } else { l };
        unsafe { PCM_RING.add((written % PCM_FRAMES) as usize).write_volatile([l, r]) };
        written = written.wrapping_add(1);
    }
    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
    unsafe { PCM_HEADER.add(2).write_volatile(written) };
    pending.drain(..whole * frame_bytes);
    take
}

pub const ROW_Y: usize = 260;
pub const ROW_H: usize = 80;
const ROWS: usize = (HOME_Y - 40 - ROW_Y) / ROW_H;
const BG_ROW: u32 = 0xff231d2e;
pub const HOME_Y: usize = 1380;

impl Runtime {
    /// Titles from /apps/.home ("name<TAB>title" lines), for the apps that have one.
    fn titles(&self) -> Vec<(String, String)> {
        let Some(home) = self.fs.contents("/apps/.home") else { return Vec::new() };
        core::str::from_utf8(home)
            .unwrap_or("")
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(n, t)| (String::from(n), String::from(t)))
            .collect()
    }
    /// Programs under /apps: titled apps first, then the rest, each sorted.
    pub fn app_entries(&self) -> Vec<String> {
        let titled: Vec<String> = self.titles().into_iter().map(|(n, _)| n).collect();
        let others = self.fs.list("/apps").into_iter().map(|(n, _)| n)
            .filter(|n| !n.starts_with('.') && !titled.contains(n) && alloc::format!("/apps/{}", n) != super::HOME_APP);
        titled.iter().cloned().filter(|n| self.fs.stat(&alloc::format!("/apps/{}", n)).is_some())
            .chain(others)
            .map(|n| alloc::format!("/apps/{}", n))
            .collect()
    }

    /// Start the Home app, if there is one. It stays running; "home" shows it.
    pub fn start_home(&mut self) {
        if self.fs.stat(super::HOME_APP).is_none() {
            return;
        }
        match self.launch(super::HOME_APP, &[]) {
            Ok(i) => {
                self.home = Some(i);
                self.foreground = Some(i);
            }
            Err(e) => crate::log!("could not start Home: {}", e),
        }
    }
    /// Show the Home app when nothing else is in front (or the list without one).
    pub fn show_home(&mut self) {
        match self.home {
            Some(h) if self.apps[h].is_some() => {
                if self.foreground != Some(h) {
                    self.foreground = Some(h);
                    self.refresh(h);
                    self.lifecycle(h, LIFECYCLE_FOCUS_GAINED);
                }
            }
            _ => self.shell.dirty = true,
        }
    }

    pub fn draw_shell(&mut self) {
        self.shell.dirty = false;
        fill(0, 0, WIDTH, HEIGHT, BG);
        text(42, 60, "VIOLET OS", 3, ACCENT);
        text(42, 110, "OPEN APPS", 8, INK);
        let entries = self.app_entries();
        if entries.is_empty() {
            text(42, ROW_Y, "NO APPS IN /APPS", 4, INK);
        }
        let titles = self.titles();
        for (i, path) in entries.iter().take(ROWS).enumerate() {
            let y = ROW_Y + i * ROW_H;
            let name = path.trim_start_matches("/apps/");
            let title = titles.iter().find(|(n, _)| n == name).map(|(_, t)| t.to_uppercase());
            fill(42, y, WIDTH - 84, ROW_H - 12, if title.is_some() { PANEL } else { BG_ROW });
            let shown: String = title.as_deref().unwrap_or(name).chars().take(16).collect();
            text(72, y + 16, &shown, if title.is_some() { 5 } else { 4 }, INK);
            if self.apps.iter().flatten().any(|a| a.path == *path) {
                text(WIDTH - 72 - 7 * 2 * 8, y + 28, "RUNNING", 2, ACCENT);
            }
        }
        if !self.shell.message.is_empty() {
            let m: String = self.shell.message.chars().take(36).collect();
            text(42, HOME_Y - 50, &m, 3, ACCENT);
        }
        fill(42, HOME_Y, WIDTH - 84, 150, PANEL);
        text(78, HOME_Y + 54, "HOME", 7, INK);
        sys::clean_framebuffer(0, HEIGHT);
    }

    fn shell_tap(&mut self, x: i32, y: i32) {
        let y = y as usize;
        if (HOME_Y..HOME_Y + 150).contains(&y) {
            if self.home.is_some() {
                self.show_home();
            } else {
                request(EXECUTE, OPEN, LAUNCHER);
            }
            return;
        }
        if y < ROW_Y || x < 42 || x > (WIDTH - 42) as i32 {
            return;
        }
        let row = (y - ROW_Y) / ROW_H;
        let Some(path) = self.app_entries().into_iter().nth(row) else { return };
        if let Some(i) = (0..MAX_APPS).find(|&i| self.apps[i].as_ref().map_or(false, |a| a.path == path)) {
            self.foreground = Some(i);
            self.refresh(i);
            self.lifecycle(i, LIFECYCLE_FOCUS_GAINED);
            return;
        }
        match self.launch(&path, &[]) {
            Ok(i) => {
                self.shell.message.clear();
                self.foreground = Some(i);
                fill(0, 0, WIDTH, HEIGHT, 0xff000000);
            }
            Err(e) => {
                crate::log!("could not start {}: {}", path, e);
                self.shell.message = e;
                self.shell.dirty = true;
            }
        }
    }

    /// A touch change from Runtime, in screen pixels.
    pub fn touch(&mut self, x: i32, y: i32, down: bool) {
        if !self.shell.armed {
            self.shell.armed = !down;
            self.shell.held = down;
            return;
        }
        let Some(i) = self.foreground else {
            if down && !self.shell.held {
                self.shell_tap(x, y);
            }
            self.shell.held = down;
            return;
        };
        // Holding the top edge for a second returns to the app list.
        let now = sys::now_ns();
        if down && !self.shell.held {
            self.shell.down_at = Some((x, y, now));
        }
        let kind = match (down, self.shell.held) {
            (true, false) => EVENT_TOUCH_DOWN,
            (true, true) => EVENT_TOUCH_MOVE,
            (false, _) => EVENT_TOUCH_UP,
        };
        if let (false, Some((_, sy, t))) = (down, self.shell.down_at) {
            if sy < 48 && now - t > 1_000_000_000 {
                self.shell.held = false;
                self.shell.down_at = None;
                self.lifecycle(i, LIFECYCLE_FOCUS_LOST);
                self.foreground = None;
                self.show_home();
                return;
            }
        }
        self.shell.held = down;
        self.event(i, InputEvent { timestamp_us: now / 1000, kind, code: 0, x, y, value: 0 });
    }

    pub fn event(&mut self, i: usize, e: InputEvent) {
        if let Some(app) = self.apps[i].as_mut() {
            if app.events.len() < 256 {
                app.events.push_back(e);
            }
        }
    }
    pub fn lifecycle(&mut self, i: usize, value: i32) {
        let now = sys::now_ns() / 1000;
        if value == LIFECYCLE_FOCUS_LOST && self.audio {
            // An app that leaves the front stops recording, as with any app.
            request(EXECUTE, MICROPHONE, MIC_OFF);
        }
        self.event(i, InputEvent { timestamp_us: now, kind: EVENT_LIFECYCLE, code: 0, x: 0, y: 0, value });
    }

    /// Show the buffer an app presented last, after it comes back to the front.
    pub fn refresh(&mut self, i: usize) {
        let shown = self.apps[i].as_ref().and_then(|a| a.surface.as_ref()).map(|s| s.shown);
        match shown {
            Some(b) => self.present(i, PresentRect { x: 0, y: 0, width: WIDTH as u32, height: HEIGHT as u32, buffer_index: b, flags: 0 }),
            None => fill(0, 0, WIDTH, HEIGHT, 0xff000000),
        }
    }

    pub fn present(&mut self, i: usize, r: PresentRect) {
        let on_screen = self.focused && self.foreground == Some(i);
        let Some(s) = self.apps[i].as_mut().and_then(|a| a.surface.as_mut()) else { return };
        if r.buffer_index > 1 {
            return;
        }
        s.shown = r.buffer_index;
        if !on_screen {
            return;
        }
        let x0 = (r.x as usize).min(WIDTH);
        let x1 = (r.x as usize + r.width as usize).min(WIDTH);
        let y0 = (r.y as usize).min(HEIGHT);
        let y1 = (r.y as usize + r.height as usize).min(HEIGHT);
        let base = r.buffer_index as usize * BUFFER_BYTES;
        for y in y0..y1 {
            let mut off = base + y * STRIDE + x0 * 4;
            let end = base + y * STRIDE + x1 * 4;
            let mut dst = FRAMEBUFFER + y * STRIDE + x0 * 4;
            while off < end {
                let n = (PAGE - off % PAGE).min(end - off);
                unsafe { core::ptr::copy_nonoverlapping(s.byte(off), dst as *mut u8, n) };
                off += n;
                dst += n;
            }
        }
        sys::clean_framebuffer(y0, y1);
    }

    /// The app's screen surface, created on first open.
    pub fn surface(&mut self, i: usize) -> Result<(), i32> {
        if self.apps[i].as_ref().unwrap().surface.is_some() {
            return Ok(());
        }
        let mut pages = Vec::new();
        for _ in 0..2 * BUFFER_BYTES / PAGE {
            match self.pool.take() {
                Some(p) => pages.push(p),
                None => {
                    for p in pages {
                        self.pool.give_back(p);
                    }
                    return Err(super::syscall::ENOMEM);
                }
            }
        }
        self.apps[i].as_mut().unwrap().surface = Some(Surface { pages, shown: 0 });
        Ok(())
    }

    /// Map the surface into the app (mmap of /dev/violet/screen).
    pub fn map_surface(&mut self, i: usize, va: u32, len: u32, offset: u32) -> Result<(), i32> {
        self.surface(i)?;
        let app = self.apps[i].as_mut().unwrap();
        let pages = app.surface.as_ref().unwrap().pages.clone();
        let first = offset as usize / PAGE;
        let count = (len as usize + PAGE - 1) / PAGE;
        if first + count > pages.len() {
            return Err(super::syscall::EINVAL);
        }
        for n in 0..count {
            let at = va + (n * PAGE) as u32;
            app.space.map(&mut self.k, &self.pool, at, pages[first + n], false)?;
        }
        Ok(())
    }

    /// Whether a wait can finish now, and with what result.
    pub fn ready(&mut self, i: usize, wait: &Wait) -> Option<i32> {
        match wait {
            Wait::Futex { .. } | Wait::Sleep => None,
            Wait::Audio { fd, buf, len } => {
                let r = self.write_audio(i, *fd, *buf, *len);
                (r != 0).then_some(r)
            }
            Wait::Connect { fd } => self.connect_result(i, *fd),
            Wait::Send { fd, buf, len } => {
                let r = self.socket_send(i, *fd, *buf, *len, None);
                (r != super::syscall::EAGAIN).then_some(r)
            }
            Wait::Read { fd, buf, len } => {
                let r = self.read_now(i, *fd, *buf, *len);
                (r != super::syscall::EAGAIN).then_some(r)
            }
            Wait::Poll { fds, ptr, time64: _ } => {
                let mut n = 0;
                let mut out = Vec::new();
                for &(fd, events) in fds {
                    let rev = self.poll_mask(i, fd) & (events as u32 | 0x18);
                    if rev != 0 {
                        n += 1;
                    }
                    out.push(rev as i16);
                }
                if n == 0 {
                    return None;
                }
                let app = self.apps[i].as_mut().unwrap();
                for (k, rev) in out.iter().enumerate() {
                    app.space.write(&mut self.k, &mut self.pool, ptr + 8 * k as u32 + 6, &rev.to_le_bytes());
                }
                Some(n)
            }
            Wait::Epoll { epfd, events, max } => {
                let n = self.epoll_collect(i, *epfd, *events, *max);
                (n > 0).then_some(n)
            }
        }
    }

    /// poll(2) bits for a descriptor: POLLIN 1, POLLOUT 4, POLLHUP 0x10, POLLNVAL 0x20.
    pub fn poll_mask(&mut self, i: usize, fd: i32) -> u32 {
        let app = self.apps[i].as_ref().unwrap();
        let Some(Some(f)) = app.fds.get(fd as usize) else { return 0x20 };
        let f = f.borrow();
        match &f.kind {
            Kind::Events => {
                if app.events.is_empty() { 4 } else { 5 }
            }
            Kind::EventFd { count } => 4 | (*count > 0) as u32,
            Kind::Pipe { buffer, write: false, peer_open } => {
                let b = buffer.borrow();
                (!b.is_empty()) as u32 | if *peer_open.borrow() == 0 { 0x11 } else { 0 }
            }
            Kind::Pipe { write: true, .. } => 4,
            Kind::Audio { .. } => if pcm_space() > 0 { 4 } else { 0 },
            Kind::Udp { replies, .. } => 4 | (!replies.is_empty()) as u32,
            Kind::Tcp { slot: None, .. } => 4,
            Kind::Tcp { slot: Some(slot), .. } => {
                let net = &self.net;
                match net.state(*slot).0 {
                    super::net::CONNECTING => 0,
                    super::net::OPEN => (net.rx_ready(*slot) > 0) as u32 | if net.tx_space(*slot) > 0 { 4 } else { 0 },
                    super::net::PEER_CLOSED => 1 | 0x10 | if net.rx_ready(*slot) > 0 { 0 } else { 0 } | 4,
                    _ => 1 | 4 | 8 | 0x10,
                }
            }
            Kind::Epoll { .. } => 0,
            Kind::Sys { pending } => 4 | (!pending.is_empty()) as u32,
            _ => 5,
        }
    }

    pub fn epoll_collect(&mut self, i: usize, epfd: i32, events: u32, max: u32) -> i32 {
        let interest = {
            let app = self.apps[i].as_ref().unwrap();
            let Some(Some(f)) = app.fds.get(epfd as usize) else { return super::syscall::EBADF };
            let f = f.borrow();
            match &f.kind {
                Kind::Epoll { interest, .. } => interest.clone(),
                _ => return super::syscall::EINVAL,
            }
        };
        const EPOLLET: u32 = 1 << 31;
        let mut n = 0u32;
        let mut edges: Vec<(i32, u32)> = Vec::new();
        for (fd, mask, data) in interest {
            if n >= max {
                break;
            }
            let ready = self.poll_mask(i, fd) & (mask | 0x18);
            let mut rev = ready;
            if mask & EPOLLET != 0 {
                // Edge-triggered writability: an idle socket stays writable, and
                // Go's poller would wake for it forever. EPOLLOUT is reported
                // once, and again after a write would block (epoll_rearm). Input
                // stays level-triggered: readers drain until EAGAIN.
                const EPOLLOUT: u32 = 4;
                let last = self.epoll_reported(i, epfd, fd);
                rev = ready & !(last & EPOLLOUT);
                edges.push((fd, ready & EPOLLOUT));
            }
            if rev == 0 {
                continue;
            }
            // struct epoll_event on ARM EABI: u32 events, 4 bytes padding, u64 data.
            let mut bytes = [0u8; 16];
            bytes[..4].copy_from_slice(&rev.to_le_bytes());
            bytes[8..].copy_from_slice(&data.to_le_bytes());
            let app = self.apps[i].as_mut().unwrap();
            app.space.write(&mut self.k, &mut self.pool, events + 16 * n, &bytes);
            n += 1;
        }
        if !edges.is_empty() {
            if let Some(Some(f)) = self.apps[i].as_ref().unwrap().fds.get(epfd as usize) {
                if let Kind::Epoll { reported, .. } = &mut f.borrow_mut().kind {
                    for (fd, ready) in edges {
                        match reported.iter_mut().find(|r| r.0 == fd) {
                            Some(r) => r.1 = ready,
                            None => reported.push((fd, ready)),
                        }
                    }
                }
            }
        }
        n as i32
    }

    /// A write to fd would block: report its writability again when it returns.
    pub fn epoll_rearm(&mut self, i: usize, fd: i32) {
        for f in self.apps[i].as_ref().unwrap().fds.iter().flatten() {
            if let Ok(mut f) = f.try_borrow_mut() {
                if let Kind::Epoll { reported, .. } = &mut f.kind {
                    reported.retain(|r| r.0 != fd);
                }
            }
        }
    }

    fn epoll_reported(&self, i: usize, epfd: i32, fd: i32) -> u32 {
        let Some(Some(f)) = self.apps[i].as_ref().unwrap().fds.get(epfd as usize) else { return 0 };
        match &f.borrow().kind {
            Kind::Epoll { reported, .. } => reported.iter().find(|r| r.0 == fd).map_or(0, |r| r.1),
            _ => 0,
        }
    }

    /// Read without blocking; EAGAIN when nothing is available yet.
    pub fn read_now(&mut self, i: usize, fd: i32, buf: u32, len: u32) -> i32 {
        use super::syscall::*;
        let f = match self.apps[i].as_ref().unwrap().fds.get(fd as usize) {
            Some(Some(f)) => f.clone(),
            _ => return EBADF,
        };
        let mut f = f.borrow_mut();
        let offset = f.offset;
        let data: Vec<u8> = match &mut f.kind {
            Kind::File { path } => {
                let Some(c) = self.fs.contents(path) else { return EIO };
                let start = (offset as usize).min(c.len());
                let end = (start + len as usize).min(c.len());
                c[start..end].to_vec()
            }
            Kind::Dir { .. } => return EISDIR,
            Kind::Null | Kind::Log => Vec::new(),
            Kind::Random => {
                let mut v = Vec::with_capacity(len as usize);
                let mut x = sys::counter() | 1;
                while v.len() < len as usize {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    v.push((x >> 24) as u8);
                }
                v
            }
            Kind::Events => {
                let app = self.apps[i].as_mut().unwrap();
                let mut v = Vec::new();
                while v.len() + 24 <= len as usize {
                    let Some(e) = app.events.pop_front() else { break };
                    let raw: [u8; 24] = unsafe { core::mem::transmute(e) };
                    v.extend_from_slice(&raw);
                }
                if v.is_empty() {
                    return if len < 24 { EINVAL } else { EAGAIN };
                }
                v
            }
            Kind::Sys { pending } => {
                if pending.is_empty() {
                    return 0;
                }
                let n = pending.len().min(len as usize);
                pending.drain(..n).collect()
            }
            Kind::Tcp { slot: None, .. } => return -107, // ENOTCONN
            Kind::Tcp { slot: Some(slot), .. } => {
                let slot = *slot;
                let (state, error) = self.net.state(slot);
                let got = self.net.recv(slot, len as usize);
                if got.is_empty() {
                    return match state {
                        super::net::PEER_CLOSED => 0,
                        super::net::FAILED => -error.max(1),
                        super::net::CONNECTING => EAGAIN,
                        _ => EAGAIN,
                    };
                }
                got
            }
            Kind::Udp { replies, .. } => {
                let Some(mut r) = replies.pop_front() else { return EAGAIN };
                r.truncate(len as usize);
                r
            }
            Kind::EventFd { count } => {
                if *count == 0 {
                    return EAGAIN;
                }
                let v = count.to_le_bytes().to_vec();
                *count = 0;
                v
            }
            Kind::Pipe { buffer, write: false, peer_open } => {
                let mut b = buffer.borrow_mut();
                if b.is_empty() {
                    return if *peer_open.borrow() == 0 { 0 } else { EAGAIN };
                }
                let n = b.len().min(len as usize);
                b.drain(..n).collect()
            }
            _ => return EBADF,
        };
        if matches!(f.kind, Kind::File { .. }) {
            f.offset += data.len() as u64;
        }
        drop(f);
        let app = self.apps[i].as_mut().unwrap();
        if !app.space.write(&mut self.k, &mut self.pool, buf, &data) {
            return EFAULT;
        }
        data.len() as i32
    }

    /// Write PCM; 0 means the ring is full and nothing was taken.
    pub fn write_audio(&mut self, i: usize, fd: i32, buf: u32, len: u32) -> i32 {
        use super::syscall::*;
        let f = match self.apps[i].as_ref().unwrap().fds.get(fd as usize) {
            Some(Some(f)) => f.clone(),
            _ => return EBADF,
        };
        let data = {
            let app = self.apps[i].as_mut().unwrap();
            let mut v = alloc::vec![0u8; len.min(64 << 10) as usize];
            if !app.space.read(&mut self.k, &mut self.pool, buf, &mut v) {
                return EFAULT;
            }
            v
        };
        let mut f = f.borrow_mut();
        let Kind::Audio { channels, pending } = &mut f.kind else { return EBADF };
        audio_write(*channels, pending, &data) as i32
    }

    /// A command written to /dev/violet/sys. Answers are read back from it.
    pub fn system(&mut self, i: usize, line: &str) -> String {
        let mut words = line.split_whitespace();
        match words.next() {
            // Back to the Home app (or, without one, the classic launcher).
            Some("home") => {
                if self.home.is_some() && self.home != Some(i) {
                    if self.foreground == Some(i) {
                        self.lifecycle(i, LIFECYCLE_FOCUS_LOST);
                    }
                    self.foreground = None;
                    self.show_home();
                    return String::from("ok\n");
                }
                if self.foreground == Some(i) {
                    self.foreground = None;
                    self.shell.dirty = true;
                }
                request(EXECUTE, OPEN, LAUNCHER);
                String::from("ok\n")
            }
            // The list of every program in /apps.
            Some("apps") => {
                if let Some(f) = self.foreground {
                    self.lifecycle(f, LIFECYCLE_FOCUS_LOST);
                }
                self.foreground = None;
                self.shell.dirty = true;
                String::from("ok\n")
            }
            // System settings: Wi-Fi, Phone Internet, packages and power.
            Some("system") => {
                request(EXECUTE, OPEN, LAUNCHER);
                String::from("ok\n")
            }
            // Bring an app to the front, starting it if it is not running.
            Some("open") => match words.next() {
                Some(path) => {
                    if let Some(n) = (0..MAX_APPS).find(|&n| self.apps[n].as_ref().map_or(false, |a| a.path == path)) {
                        if self.foreground != Some(n) {
                            if let Some(f) = self.foreground {
                                self.lifecycle(f, LIFECYCLE_FOCUS_LOST);
                            }
                            self.foreground = Some(n);
                            self.refresh(n);
                            self.lifecycle(n, LIFECYCLE_FOCUS_GAINED);
                        }
                        return String::from("ok\n");
                    }
                    match self.launch(path, &[]) {
                        Ok(n) => {
                            if let Some(f) = self.foreground {
                                self.lifecycle(f, LIFECYCLE_FOCUS_LOST);
                            }
                            self.foreground = Some(n);
                            fill(0, 0, WIDTH, HEIGHT, 0xff000000);
                            String::from("ok\n")
                        }
                        Err(e) => alloc::format!("error {}\n", e),
                    }
                }
                None => String::from("error usage: open PATH\n"),
            },
            // The programs running now, as paths.
            Some("running") => {
                let paths: Vec<&str> = self.apps.iter().flatten().map(|a| a.path.as_str()).collect();
                alloc::format!("{}\n", paths.join(" "))
            }
            Some("status") => {
                let power = request(GET, POWER, 0);
                let volume = request(GET, VOLUME, 0);
                alloc::format!(
                    "battery={} volume={} headphones={} focused={}\n",
                    if power.op == OK { power.value as i32 } else { -1 },
                    if volume.op == OK { volume.value as i32 } else { -1 },
                    (volume.op == OK && volume.arg & AUDIO_HEADPHONES != 0) as u32,
                    (self.focused && self.foreground == Some(i)) as u32
                )
            }
            Some("volume") if line.split_whitespace().nth(1).map_or(false, |w| w == "up" || w == "down") => {
                let step = if words.next() == Some("up") { VOLUME_UP } else { VOLUME_DOWN };
                let r = request(EXECUTE, VOLUME, step);
                if r.op == OK { alloc::format!("volume={}\n", r.value) } else { String::from("error\n") }
            }
            Some("volume") => {
                let level = words.next().and_then(|w| w.parse::<u32>().ok()).unwrap_or(u32::MAX);
                let r = request(EXECUTE, VOLUME, level);
                if r.op == OK { alloc::format!("volume={}\n", r.value) } else { String::from("error\n") }
            }
            // Audio state in one line: output, volume, playback and microphone.
            Some("audio") => {
                let volume = request(GET, VOLUME, 0);
                let playback = request(GET, PLAYBACK, 0);
                let mic = request(GET, MICROPHONE, 0);
                if volume.op != OK {
                    return String::from("error no audio\n");
                }
                let f = volume.arg;
                let outcome = match playback.arg & PLAYBACK_OUTCOME {
                    PLAYBACK_PLAYED => "played",
                    PLAYBACK_STOPPED => "stopped",
                    PLAYBACK_NO_OUTPUT => "no-output",
                    _ => "ready",
                };
                let mode = match mic.arg & MIC_MODE {
                    MIC_LISTEN => "listen",
                    MIC_RECORD => "record",
                    _ => "off",
                };
                alloc::format!(
                    "volume={} max={} headphones={} speaker={} headphone_out={} mic={} playing={} outcome={} mic_mode={} mic_level={} recording_ms={}\n",
                    volume.value, VOLUME_MAX, (f & AUDIO_HEADPHONES != 0) as u32, (f & AUDIO_SPEAKER_OUT != 0) as u32,
                    (f & AUDIO_HEADPHONE_OUT != 0) as u32, (f & AUDIO_MIC != 0) as u32,
                    if playback.op == OK { sound_name(playback.value) } else { "none" }, outcome, mode,
                    if mic.op == OK { mic.value } else { 0 },
                    if mic.op == OK { (mic.arg >> 16) * 10 } else { 0 }
                )
            }
            // "sound chime", "sound tone 440 1500", "sound recording", "sound stop".
            Some("sound") => {
                let name = words.next().unwrap_or("");
                let sound = match name {
                    "stop" => SOUND_STOP,
                    "click" => SOUND_CLICK,
                    "chime" => SOUND_CHIME,
                    "alert" => SOUND_ALERT,
                    "tone" => SOUND_TONE,
                    "recording" => SOUND_RECORDING,
                    _ => return String::from("error usage: sound stop|click|chime|alert|tone HZ MS|recording\n"),
                };
                let hz = words.next().and_then(|w| w.parse::<u32>().ok()).unwrap_or(440).min(0xffff);
                let ms = words.next().and_then(|w| w.parse::<u32>().ok()).unwrap_or(1500).min(0xffff);
                let arg = if sound == SOUND_TONE { hz | ms << 16 } else { 0 };
                let r = super::request_with(EXECUTE, PLAYBACK, sound, arg);
                if r.op == OK { String::from("ok\n") } else { alloc::format!("error {}\n", r.op) }
            }
            // "mic record" (up to four seconds), "mic listen", "mic off".
            Some("mic") => {
                let mode = match words.next() {
                    Some("record") => MIC_RECORD,
                    Some("listen") => MIC_LISTEN,
                    Some("off") => MIC_OFF,
                    _ => return String::from("error usage: mic record|listen|off\n"),
                };
                let r = request(EXECUTE, MICROPHONE, mode);
                if r.op == OK { String::from("ok\n") } else { alloc::format!("error {}\n", r.op) }
            }
            Some("launch") => match words.next() {
                Some(path) => match self.launch(path, &[]) {
                    Ok(n) => {
                        self.foreground = Some(n);
                        String::from("ok\n")
                    }
                    Err(e) => alloc::format!("error {}\n", e),
                },
                None => String::from("error usage: launch PATH\n"),
            },
            Some("log") => {
                let rest: Vec<&str> = words.collect();
                crate::log!("{}", rest.join(" "));
                String::from("ok\n")
            }
            _ => String::from("error unknown command\n"),
        }
    }
}

#[allow(dead_code)]
fn _size_check() {
    let _: [u8; 24] = unsafe { core::mem::transmute(InputEvent::default()) };
}

fn sound_name(sound: u32) -> &'static str {
    match sound {
        SOUND_CLICK => "click",
        SOUND_CHIME => "chime",
        SOUND_ALERT => "alert",
        SOUND_TONE => "tone",
        SOUND_RECORDING => "recording",
        SOUND_STREAM => "stream",
        _ => "none",
    }
}
