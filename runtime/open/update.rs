//! Full-system updates: the `update` commands of /dev/violet/sys, for
//! Settings (which fetches a signed bundle over HTTPS) and the Agent (which
//! may push one). Either app puts each piece of the bundle, at most 64 KiB,
//! in its own /data/NAME.ota-staging (`staging`: only that app can write or
//! read it, policy.rs `owns_data`; the name is too long for /data to keep it
//! across restarts) and then:
//!
//!   update info               "slot=SECTORS running=a|b trial=0|1", or "slot=0"
//!                             (no updates); trial=1: the running slot is not yet
//!                             marked successful, and Storage refuses to write
//!   update manifest           the 512-byte manifest (Storage holds it until apply)
//!   update check              the supervisor checks the manifest: "ok version=V system=S"
//!   update write SECTOR       the piece into the inactive slot from SECTOR
//!   update hash FIRST COUNT   Storage reads back and hashes COUNT (<= 2048) sectors
//!   update apply              the supervisor checks signature, version and the
//!                             read-back digest and makes the slot the next boot
//!
//! A refusal answers "error refused=N system=S" (N as in ota_verify,
//! supervisor.c). Only the supervisor can make a slot bootable: whatever an
//! app writes here is booted only if it is a signed system for this phone,
//! no older than the running one (system/package-manager/ota.rs).
use crate::protocol::*;
use alloc::string::String;

use super::fs::Fs;

/// Where `app` (/apps/NAME) stages each piece: /data/NAME.ota-staging.
pub fn staging(app: &str) -> String {
    alloc::format!("/data/{}.ota-staging", app.trim_start_matches("/apps/"))
}
const WINDOW: *mut u8 = 0x0913_0000 as *mut u8;
const WINDOW_BYTES: usize = 128 * 512;
/// In APPSTORE's value: the inactive boot slot (services/storage/storage.rs).
const SLOT: u32 = 1 << 31;
const HASH: u32 = 1 << 30;
const CHECK: u32 = 0x3141_544f; // "OTA1"
const APPLY: u32 = 0x3241_544f; // "OTA2"
/// The apps that may update the system.
pub fn allowed(path: &str) -> bool {
    path == super::wifi::SETTINGS_APP || path == super::appstore::INSTALLER
}

fn storage(op: u32, value: u32, count: u32) -> Message {
    let r = Message::new(op, APPSTORE, value).with_arg(count).call(super::sys::SLOT_RUNTIME);
    if r.op != OK {
        return r;
    }
    loop {
        let p = Message::new(POLL, 0, r.value).call(super::sys::SLOT_RUNTIME);
        if p.op != BUSY {
            return p;
        }
        super::sys::pause();
    }
}

fn number(w: Option<&str>) -> Option<u32> {
    w.and_then(|w| w.parse::<u32>().ok()).filter(|&v| v < HASH)
}

/// Copy the staged piece into the window and write it at `sector`.
fn write(fs: &mut Fs, chunk: &str, sector: u32, exact: Option<usize>) -> Result<u32, &'static str> {
    let data = fs.contents(chunk).ok_or("nothing staged")?;
    if data.is_empty() || data.len() > WINDOW_BYTES || exact.is_some_and(|n| n != data.len()) {
        return Err("bad piece size");
    }
    let n = data.len().div_ceil(512);
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), WINDOW, data.len());
        core::ptr::write_bytes(WINDOW.add(data.len()), 0, n * 512 - data.len());
    }
    let _ = fs.unlink(chunk, false);
    if storage(EXECUTE, SLOT | sector, n as u32).op != OK {
        return Err("write failed");
    }
    Ok(n as u32)
}

pub fn command<'a>(path: &str, fs: &mut Fs, mut words: impl Iterator<Item = &'a str>) -> String {
    if !allowed(path) {
        return String::from("error only Settings and the agent update the system\n");
    }
    let info = || {
        let r = storage(GET, SLOT, 0);
        if r.op == OK { r.value } else { 0 }
    };
    let chunk = staging(path);
    const SIZE: u32 = !(3 << 30);
    let r = match words.next() {
        Some("info") => {
            let v = info();
            return if v & SIZE == 0 {
                String::from("slot=0\n")
            } else {
                alloc::format!("slot={} running={} trial={}\n", v & SIZE, if v >> 31 == 0 { 'a' } else { 'b' }, v >> 30 & 1)
            };
        }
        Some("manifest") => match info() & SIZE {
            0 => Err("no slot"),
            cap => write(fs, &chunk, cap - 1, Some(512)),
        },
        Some("write") => match number(words.next()) {
            Some(sector) => write(fs, &chunk, sector, None),
            None => Err("usage: update write SECTOR"),
        },
        Some("hash") => match (number(words.next()), number(words.next())) {
            (Some(first), Some(count)) => {
                let r = storage(EXECUTE, SLOT | HASH | first, count);
                if r.op == OK { Ok(r.value) } else { Err("hash failed") }
            }
            _ => Err("usage: update hash FIRST COUNT"),
        },
        Some(verb @ ("check" | "apply")) => {
            let r = super::request(EXECUTE, INSTALL, if verb == "apply" { APPLY } else { CHECK });
            crate::log!("update {}: {} {}", verb, if r.op == OK { "version" } else { "refused" }, r.value);
            return if r.op == OK {
                alloc::format!("ok version={} system={}\n", r.value, r.arg)
            } else {
                alloc::format!("error refused={} system={}\n", r.value, r.arg)
            };
        }
        _ => Err("usage: update info|manifest|check|write|hash|apply"),
    };
    match r {
        Ok(n) => alloc::format!("ok {}\n", n),
        Err(e) => alloc::format!("error {}\n", e),
    }
}
