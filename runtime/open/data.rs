//! /data: files that survive a restart. Each file directly in /data is kept
//! by the Storage service in the open apps' private directory (four files of
//! up to 4 KiB, names of up to 16 letters, digits, '.', '_' or '-'). The
//! runtime loads them at startup and writes a file back after it changes.
use crate::log;
use crate::protocol::*;
use alloc::string::String;
use alloc::vec::Vec;

use super::fs::Fs;

pub const DIR: &str = "/data";
const MAX: usize = 4096;

/// The Storage name for a path, if the path is one /data can keep.
pub fn key(path: &str) -> Option<&str> {
    let name = path.strip_prefix("/data/")?;
    let ok = !name.is_empty()
        && name.len() <= 16
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    ok.then_some(name)
}

fn file(op: u32, key: u32, name: &str, offset: u32, data: &[u8]) -> Message {
    let mut m = Message::new(op, key, offset);
    m.arg = data.len() as u32;
    let mut bytes = [0u8; 40];
    bytes[..name.len()].copy_from_slice(name.as_bytes());
    bytes[16..16 + data.len()].copy_from_slice(data);
    m.put_text(&bytes);
    let r = m.call(super::sys::SLOT_RUNTIME);
    if r.op != OK {
        return r;
    }
    loop {
        let p = Message::new(POLL, 0, r.value).call(super::sys::SLOT_RUNTIME);
        if p.op != BUSY {
            return p;
        }
        super::sys::yield_now();
    }
}

/// Put every stored file into /data.
pub fn load(fs: &mut Fs) {
    let _ = fs.mkdir(DIR);
    let mut count = 0;
    for slot in 0..4 {
        let entry = file(GET, FILE_LIST, "", slot, &[]);
        if entry.op != OK || entry.value == 0 {
            continue;
        }
        let raw = entry.text();
        let end = raw[..16].iter().position(|&b| b == 0).unwrap_or(16);
        let Ok(name) = core::str::from_utf8(&raw[..end]) else { continue };
        let name = String::from(name);
        let len = entry.value as usize;
        let mut data = Vec::with_capacity(len);
        while data.len() < len {
            let r = file(GET, FILE_READ, &name, data.len() as u32, &[]);
            if r.op != OK || r.value == 0 {
                break;
            }
            data.extend_from_slice(&r.text()[..r.value as usize]);
        }
        let path = alloc::format!("{}/{}", DIR, name);
        if let Some(f) = fs.writable(&path, true, true, 0) {
            *f = data;
            count += 1;
        }
    }
    fs.changed.clear();
    log!("/data: {} saved files", count);
}

/// Write back files in /data that changed (or delete the ones removed).
pub fn flush(fs: &mut Fs) {
    if fs.changed.is_empty() {
        return;
    }
    let changed = core::mem::take(&mut fs.changed);
    for path in changed {
        let Some(name) = key(&path) else {
            log!("{} is not kept across restarts (names in /data: up to 16 of A-Z a-z 0-9 . _ -)", path);
            continue;
        };
        match fs.contents(&path).map(|d| d.to_vec()) {
            None => {
                file(EXECUTE, FILE_DELETE, name, 0, &[]);
            }
            Some(data) if data.len() > MAX => log!("{} is over 4 KiB and is not kept", path),
            Some(data) => {
                let old = file(GET, FILE_READ, name, 0, &[]);
                // Rewrite only what differs; truncate first if it shrank.
                if old.op == OK && (old.arg as usize) > data.len() {
                    file(EXECUTE, FILE_TRUNCATE, name, data.len() as u32, &[]);
                }
                let mut ok = true;
                for (i, chunk) in data.chunks(24).enumerate() {
                    let r = file(EXECUTE, FILE_WRITE, name, (i * 24) as u32, chunk);
                    if r.op != OK {
                        ok = false;
                        break;
                    }
                }
                if data.is_empty() && old.op != OK {
                    ok = file(EXECUTE, FILE_WRITE, name, 0, &[]).op == OK;
                }
                if !ok {
                    log!("could not save {} (Storage keeps four files for open apps)", path);
                }
            }
        }
    }
}
