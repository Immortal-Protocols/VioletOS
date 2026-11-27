use std::cell::Cell;
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

thread_local! { static ID: Cell<u32> = const { Cell::new(0) }; }

fn main() {
    ID.set(99);
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for id in 1..=2 {
        let b = barrier.clone();
        handles.push(thread::spawn(move || {
            ID.set(id);
            b.wait();
            thread::sleep(Duration::from_millis(10));
            ID.get()
        }));
    }
    barrier.wait();
    for (id, handle) in handles.into_iter().enumerate() {
        assert_eq!(handle.join().unwrap(), id as u32 + 1);
    }
    assert_eq!(ID.get(), 99);
    let lock = Mutex::new(false);
    let cv = Condvar::new();
    let before = Instant::now();
    let (_guard, timeout) = cv.wait_timeout(lock.lock().unwrap(), Duration::from_millis(15)).unwrap();
    assert!(timeout.timed_out());
    assert!(before.elapsed() >= Duration::from_millis(14));
    // Exercise more sequential creations than the concurrent thread limit.
    for n in 0..40 {
        assert_eq!(thread::spawn(move || n).join().unwrap(), n);
    }
    println!("AArch64 threads: TLS, futex, timer, join and TCB reuse passed");
}
