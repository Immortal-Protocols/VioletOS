//! Notes (Violet Shield demo, benign): keeps two notes, one holding an API
//! key, in its own folder /data/notes, then syncs a short summary to its own
//! server, sync.notes.test. The companion maps that name to a stand-in, so
//! nothing reaches a real address.
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use violet::{colors::*, Canvas, Fonts, Rect, Screen, W};

const KEYS: &str = "weather_api_key=8f2b1c0e9a7d\nbank_pin_hint=dog's birthday\n";
const TODO: &str = "buy milk\ncall mom\n";

fn sync() -> std::io::Result<String> {
    let addr = ("sync.notes.test", 80).to_socket_addrs()?.next().ok_or(std::io::ErrorKind::NotFound)?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(20))?;
    s.set_read_timeout(Some(Duration::from_secs(20)))?;
    s.write_all(b"POST /sync HTTP/1.0\r\nHost: sync.notes.test\r\nContent-Length: 18\r\n\r\n{\"notes\": 2, \"v\":1}")?;
    let mut reply = [0u8; 200];
    let n = s.read(&mut reply)?;
    Ok(String::from_utf8_lossy(&reply[..n]).lines().next().unwrap_or("").to_string())
}

fn main() {
    let mut screen = Screen::open().expect("no screen");
    let fonts = Fonts::load();
    let mut c = screen.canvas();
    let draw = |c: &mut Canvas, notes: &[&str], status: &str| {
        c.fill(c.bounds(), BACKGROUND);
        violet::header(c, &fonts, "Notes");
        for (i, n) in notes.iter().enumerate() {
            let r = Rect::new(40, 240 + i as i32 * 230, W as i32 - 40, 440 + i as i32 * 230);
            c.round_rect(r, 28.0, PANEL);
            for (k, line) in n.lines().enumerate() {
                fonts.medium(32.0).draw(c, line, r.x0 + 34, r.y0 + 70 + k as i32 * 52, INK);
            }
        }
        fonts.medium(30.0).centered(c, status, 0, W as i32, 1400, MUTED);
    };
    let notes = [KEYS, TODO];
    draw(&mut c, &notes, "Saving...");
    screen.show_all(&c);

    std::fs::create_dir_all("/data/notes").ok();
    std::fs::write("/data/notes/keys.txt", KEYS).expect("keys");
    std::fs::write("/data/notes/todo.txt", TODO).expect("todo");
    let items = std::fs::read_to_string("/data/notes/todo.txt").unwrap_or_default().lines().count();
    println!("notes: {items} notes saved");
    draw(&mut c, &notes, "Syncing with sync.notes.test...");
    screen.show_all(&c);
    let status = match sync() {
        Ok(reply) => {
            println!("notes: synced -> {reply}");
            "Synced".to_string()
        }
        Err(e) => {
            println!("notes: sync failed: {e}");
            format!("Sync failed: {e}")
        }
    };
    draw(&mut c, &notes, &status);
    screen.show_all(&c);
    std::thread::sleep(Duration::from_secs(2));
    println!("notes done");
}
