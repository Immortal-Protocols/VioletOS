//! Present the full upstream Hub's CPU-rendered terminal view on VioletOS.
//! The browser writes RGB frames into its own guest temporary directory and
//! receives pixel mouse reports over a raw PTY. All helpers run on seL4.
use super::{ADDRESS_BOX, BACK_BOX, TOOLBAR};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use violet::device::{self, TOUCH_DOWN, TOUCH_MOVE, TOUCH_UP};
use violet::keyboard::{Key, Keyboard};
use violet::{Canvas, Fonts, H, Rect, Screen, W, colors};

const HUB: &str = "/opt/ladybird/libexec/Hub";
const BODY: Rect = Rect::new(0, TOOLBAR, W as i32, H as i32);
unsafe extern "C" {
    fn ioctl(fd: i32, cmd: usize, ...) -> i32;
}

pub fn available() -> bool {
    std::path::Path::new(HUB).is_file()
}

fn control<T>(f: &File, cmd: usize, data: &mut T) -> io::Result<()> {
    if unsafe { ioctl(f.as_raw_fd(), cmd, data) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn terminal() -> io::Result<(File, File)> {
    let master = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/ptmx")?;
    control(&master, 0x40045431, &mut 0i32)?;
    let mut num = 0u32;
    control(&master, 0x80045430, &mut num)?;
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!("/dev/pts/{num}"))?;
    // The terminal backend reserves one cell row for status. Leave the native toolbar above the web page.
    let mut window = [83u16, 90, W as u16, (H as i32 - TOOLBAR + 18) as u16];
    control(&master, 0x5414, &mut window)?;
    Ok((master, slave))
}
fn decode_path(encoded: &[u8]) -> Option<String> {
    let mut path = Vec::new();
    let mut value = 0u32;
    let mut bits = 0;
    for &c in encoded {
        if c == b'=' {
            break;
        }
        let digit = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        value = value << 6 | u32::from(digit);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            path.push((value >> bits) as u8);
        }
    }
    String::from_utf8(path).ok()
}
struct Update {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}
fn frame(cmd: &[u8], pid: u32) -> Option<Update> {
    let split = cmd.iter().position(|b| *b == b';')?;
    let keys = std::str::from_utf8(&cmd[..split]).ok()?;
    let field = |key: &str| {
        keys.split(',')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    };
    if field("t") != Some("t") || field("f") != Some("24") || !matches!(field("a"), Some("T" | "f"))
    {
        return None;
    }
    let num = |key: &str| field(key).and_then(|v| v.parse::<usize>().ok());
    let width = num("s")?;
    let height = num("v")?;
    let x = num("x").unwrap_or(0);
    let y = num("y").unwrap_or(0);
    if width == 0
        || height == 0
        || x.checked_add(width)? > W
        || y.checked_add(height)? > BODY.height() as usize
    {
        return None;
    }
    let path = decode_path(&cmd[split + 1..])?;
    let suffix = path
        .strip_prefix(&format!("/tmp/tty-graphics-protocol-ladybird-{pid}-"))?
        .strip_suffix(".rgb")?;
    if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let file = File::open(&path).ok()?;
    // The terminal graphics protocol gives the receiving terminal ownership of temporary files.
    let _ = fs::remove_file(&path);
    let size = width * height * 3;
    if file.metadata().ok()?.len() != size as u64 {
        return None;
    }
    let mut pixels = Vec::with_capacity(size);
    file.take(size as u64).read_to_end(&mut pixels).ok()?;
    (pixels.len() == size).then_some(Update {
        x,
        y,
        width,
        height,
        pixels,
    })
}
fn read_frames(mut master: File, pid: u32, output: mpsc::SyncSender<Update>) {
    let mut bytes = [0u8; 4096];
    let mut cmd = Vec::new();
    let mut state = 0;
    while let Ok(n) = master.read(&mut bytes) {
        if n == 0 {
            break;
        }
        for &b in &bytes[..n] {
            state = match (state, b) {
                (0, 27) => 1,
                (1, b'_') => 2,
                (2, b'G') => {
                    cmd.clear();
                    3
                }
                (3, 27) => 4,
                (3, _) if cmd.len() < 2048 => {
                    cmd.push(b);
                    3
                }
                (4, b'\\') => {
                    if let Some(update) = frame(&cmd, pid)
                        && output.send(update).is_err()
                    {
                        return;
                    }
                    0
                }
                (_, 27) => 1,
                _ => 0,
            }
        }
    }
}
fn draw_chrome(c: &mut Canvas, f: &Fonts, address: &str, editing: bool) {
    c.fill(Rect::new(0, 0, W as i32, TOOLBAR), colors::BACKGROUND);
    violet::button(
        c,
        &f.medium(24.0),
        violet::HOME_BOX,
        "‹ Home",
        colors::PANEL,
        colors::INK,
    );
    violet::button(c, &f.bold(34.0), BACK_BOX, "‹", colors::PANEL, colors::INK);
    c.round_rect(ADDRESS_BOX, 22.0, colors::PANEL);
    let font = f.regular(24.0);
    let mut label = address;
    while label.len() > 1 && font.width(label) > ADDRESS_BOX.width() - 40 {
        if editing {
            label = &label[label.chars().next().unwrap().len_utf8()..];
        } else {
            label = &label[..label.len() - label.chars().next_back().unwrap().len_utf8()];
        }
    }
    font.draw(
        c,
        label,
        ADDRESS_BOX.x0 + 20,
        ADDRESS_BOX.y0 + (ADDRESS_BOX.height() + font.ascent() - font.descent()) / 2,
        colors::INK,
    );
}
pub fn run(url: &str, trace: bool) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let home = "/tmp/violet-browser-home";
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(format!("{home}/.cache"))?;
    fs::create_dir_all("/tmp/fontconfig-cache")?;
    let (mut master, slave) = terminal()?;
    let mut child = Command::new(HUB)
        .env("HOME", home)
        .args([
            "--headless=terminal",
            "--force-cpu-painting",
            "--temporary-profile",
            url,
        ])
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave))
        .spawn()?;
    let (tx, frames) = mpsc::sync_channel(1);
    let reader = master.try_clone()?;
    let pid = child.id();
    std::thread::spawn(move || read_frames(reader, pid, tx));
    let mut screen = Screen::open()?;
    let events = device::events()?;
    let fonts = Fonts::load();
    let mut canvas = screen.canvas();
    let mut address = url.to_owned();
    let mut editing = false;
    let mut keyboard = Keyboard::address();
    canvas.fill(canvas.bounds(), colors::BACKGROUND);
    draw_chrome(&mut canvas, &fonts, &address, false);
    screen.show_all(&canvas);
    let mut pixels = vec![0xff_ff_ff_ff; W * BODY.height() as usize];
    let mut seen = false;
    let mut log_file = None;
    let mut log_check = std::time::Instant::now();
    loop {
        if trace && log_check.elapsed() >= Duration::from_millis(500) {
            log_check = std::time::Instant::now();
            if log_file.is_none() {
                log_file = File::open(format!("/tmp/ladybird-terminal-{pid}.log")).ok();
            }
            if let Some(log) = log_file.as_mut() {
                let _ = io::copy(&mut log.take(16384), &mut io::stderr());
            }
        }
        let mut dirty = false;
        if let Ok(f) = frames.try_recv() {
            for (i, row) in f
                .pixels
                .chunks_exact(f.width * 3)
                .take(f.height)
                .enumerate()
            {
                let dst = &mut pixels[(f.y + i) * W + f.x..][..f.width];
                for (out, rgb) in dst.iter_mut().zip(row.chunks_exact(3)) {
                    *out = violet::rgb(rgb[0], rgb[1], rgb[2]);
                }
                if !editing {
                    let offset = (TOOLBAR as usize + f.y + i) * W + f.x;
                    canvas.px[offset..offset + f.width].copy_from_slice(dst);
                }
            }
            if editing {
                dirty = true;
            } else {
                screen.show(
                    &canvas,
                    Rect::new(
                        f.x as i32,
                        TOOLBAR + f.y as i32,
                        (f.x + f.width) as i32,
                        TOOLBAR + (f.y + f.height) as i32,
                    ),
                );
            }
            if !seen {
                println!(
                    "ladybird: presented upstream compositor frame {}x{}",
                    f.width, f.height
                );
                master.write_all(b"\x1b_ladybird;frames;damage\x1b\\")?;
                seen = true;
            }
        }
        if let Ok(e) = events.recv_timeout(Duration::from_millis(12)) {
            if e.focus_gained() {
                dirty = true;
            }
            if matches!(e.kind, TOUCH_DOWN | TOUCH_UP | TOUCH_MOVE) {
                if e.y < TOOLBAR {
                    if e.kind == TOUCH_UP {
                        if violet::HOME_BOX.contains(e.x, e.y) {
                            violet::home();
                        }
                        if BACK_BOX.contains(e.x, e.y) {
                            master.write_all(b"\x1b_ladybird;back\x1b\\")?;
                        }
                        if ADDRESS_BOX.contains(e.x, e.y) {
                            editing = true;
                            address.clear();
                            dirty = true;
                        }
                    }
                } else if editing {
                    if e.kind == TOUCH_UP
                        && let Some(k) = keyboard.key_at(e.x, e.y)
                    {
                        if matches!(k, Key::Go) {
                            if !address.chars().any(char::is_control)
                                && let Some(u) = super::interpret(&address)
                            {
                                address = u.document();
                                write!(master, "\x1b_ladybird;open;{}\x1b\\", address)?;
                            }
                            editing = false;
                        } else {
                            keyboard.press(k, &mut address, 2048);
                        }
                        dirty = true;
                    }
                } else {
                    let button = if e.kind == TOUCH_MOVE { 32 } else { 0 };
                    write!(
                        master,
                        "\x1b[<{button};{};{}{}",
                        e.x + 1,
                        e.y - TOOLBAR + 1,
                        if e.kind == TOUCH_UP { 'm' } else { 'M' }
                    )?;
                }
            }
        }
        if dirty {
            canvas.px[TOOLBAR as usize * W..].copy_from_slice(&pixels);
            draw_chrome(&mut canvas, &fonts, &address, editing);
            if editing {
                keyboard.draw(&mut canvas, &fonts);
            }
            screen.show_all(&canvas);
        }
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            let log = format!("/tmp/ladybird-terminal-{pid}.log");
            if let Ok(mut file) = File::open(log) {
                if let Ok(info) = file.metadata() {
                    let _ = file.seek(SeekFrom::Start(info.len().saturating_sub(8192)));
                }
                let _ = io::copy(&mut file.take(8192), &mut io::stderr());
            }
            return Err(io::Error::other(format!("Hub exited with {status}")));
        }
    }
}
