//! Draws through /dev/violet/screen and answers touches from /dev/violet/events,
//! using only the Linux file API: open, ioctl, mmap, write and read.
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;

const W: usize = 720;
const H: usize = 1600;
extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn ioctl(fd: i32, req: u32, ...) -> i32;
}

fn present(screen: &mut std::fs::File, buffer: u32, y: u32, h: u32) {
    let rect = [0u32, y, W as u32, h, buffer, 0];
    let bytes: Vec<u8> = rect.iter().flat_map(|w| w.to_le_bytes()).collect();
    screen.write_all(&bytes).unwrap();
}

fn main() {
    let mut screen = OpenOptions::new().read(true).write(true).open("/dev/violet/screen").unwrap();
    let mut info = [0u32; 6];
    let r = unsafe { ioctl(screen.as_raw_fd(), 0x5601, info.as_mut_ptr()) };
    println!("canvas: screen info {:?} (ioctl {})", info, r);
    let bytes = info[5] as usize;
    let fb = unsafe { mmap(std::ptr::null_mut(), bytes, 3, 1, screen.as_raw_fd(), 0) };
    assert!(!fb.is_null() && (fb as isize) != -1);
    let pixels = unsafe { std::slice::from_raw_parts_mut(fb as *mut u32, W * H) };
    for y in 0..H {
        let shade = (y * 255 / H) as u32;
        for x in 0..W {
            pixels[y * W + x] = 0xff000000 | (shade << 16) | (0x30 << 8) | (255 - shade);
        }
    }
    for y in 0..160 {
        for x in 0..W {
            pixels[y * W + x] = 0xff2a7f62;
        }
    }
    present(&mut screen, 0, 0, H as u32);
    println!("canvas: drew gradient");
    let mut events = std::fs::File::open("/dev/violet/events").unwrap();
    let mut raw = [0u8; 24];
    let mut taps = 0;
    loop {
        events.read_exact(&mut raw).unwrap();
        let kind = u16::from_le_bytes([raw[8], raw[9]]);
        let x = i32::from_le_bytes(raw[12..16].try_into().unwrap());
        let y = i32::from_le_bytes(raw[16..20].try_into().unwrap());
        let value = i32::from_le_bytes(raw[20..24].try_into().unwrap());
        if kind == 7 {
            println!("canvas: lifecycle {}", value);
            continue;
        }
        if kind != 1 {
            continue;
        }
        taps += 1;
        println!("canvas: tap {} at {},{}", taps, x, y);
        if y < 160 {
            let mut sys = OpenOptions::new().read(true).write(true).open("/dev/violet/sys").unwrap();
            sys.write_all(b"home\n").unwrap();
            continue;
        }
        let (cx, cy) = (x.clamp(40, W as i32 - 41) as usize, y.clamp(200, H as i32 - 41) as usize);
        for yy in cy - 40..cy + 40 {
            for xx in cx - 40..cx + 40 {
                pixels[yy * W + xx] = 0xffffffff;
            }
        }
        present(&mut screen, 0, (cy - 40) as u32, 80);
    }
}
