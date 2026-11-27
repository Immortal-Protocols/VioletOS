//! A plain Rust `std` program, built for armv7-unknown-linux-musleabihf with no
//! VioletOS code: threads, a mutex and condition variable, files, the clock,
//! the environment and the heap all go through ordinary Linux system calls.
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

mod socketpair;

fn main() {
    socketpair::run();
    println!("hello from rust std on violet");
    let v: Vec<u64> = (1..=20).map(|n| n * n).collect();
    println!("sum of squares: {}", v.iter().sum::<u64>());

    let pair = Arc::new((Mutex::new(0u32), Condvar::new()));
    let workers: Vec<_> = (0..4)
        .map(|n| {
            let pair = pair.clone();
            std::thread::spawn(move || {
                let (lock, cvar) = &*pair;
                *lock.lock().unwrap() += n + 1;
                cvar.notify_all();
                n * 10
            })
        })
        .collect();
    let results: Vec<u32> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    let (lock, cvar) = &*pair;
    let total = cvar.wait_while(lock.lock().unwrap(), |t| *t < 10).unwrap();
    println!("threads returned {:?}, shared total {}", results, *total);

    std::fs::create_dir_all("/tmp/hello").unwrap();
    std::fs::File::create("/tmp/hello/note.txt").unwrap().write_all(b"written by hello").unwrap();
    let mut back = String::new();
    std::fs::File::open("/tmp/hello/note.txt").unwrap().read_to_string(&mut back).unwrap();
    println!("file round trip: {}", back);
    let names: Vec<String> = std::fs::read_dir("/apps").unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    println!("apps: {}", names.join(","));

    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(50));
    println!("slept {} ms", start.elapsed().as_millis());
    println!("HOME={}", std::env::var("HOME").unwrap_or_default());
    let big = vec![7u8; 8 << 20];
    println!("8 MiB allocation ok: {}", big.iter().all(|&b| b == 7));
    println!("hello done");
}
