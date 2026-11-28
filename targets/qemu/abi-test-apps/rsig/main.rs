//! Rust std under the Open Runtime (make test-system32): a signal handler through
//! musl's signal(3), std::process::Command (posix_spawn, pipes, exit status)
//! and a program that does not exist.
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
    fn raise(sig: i32) -> i32;
}
static HITS: AtomicU32 = AtomicU32::new(0);
extern "C" fn on_usr1(_: i32) {
    HITS.fetch_add(1, Ordering::SeqCst);
}

fn main() {
    unsafe {
        signal(10, on_usr1 as usize);
        raise(10);
    }
    println!("rsig: handler ran {}", HITS.load(Ordering::SeqCst));
    let status = Command::new("/apps/abichild").arg("rust").status();
    println!("rsig: child exit {:?}", status.ok().and_then(|s| s.code()));
    let out = Command::new("/apps/abichild").arg("piped").stdout(Stdio::piped()).output();
    println!("rsig: child said {:?}", out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default());
    println!("rsig: missing program refused {}", Command::new("/apps/no-such-program").status().is_err());
    println!("rsig done");
}
