//! Private files and a durable checkpoint per application. Sectors 0 and 1 retain the
//! original stopwatch journal layout. Launcher uses sectors 2–3 and Wi-Fi Setup uses 4–5.
#[path = "files.rs"]
mod files;
use crate::protocol::*;
use crate::{
    block::Disk,
    journal::{recover, Record},
};
#[cfg(package_catalog)]
mod catalog {
    use crate::block::PackageDisk as Disk;
    mod volume { include!(env!("VIOLET_CATALOG_VOLUME_RS")); }
    pub fn commit(length: u32, bank: u32) -> u32 {
        unsafe { volume::commit_bank(0x0d100000 as *const u8, length as usize, bank) }
    }
}
extern "C" {
    fn storage_reply(ok: bool, value: u32);
    fn clock_init();
}

/// The 64 KiB window shared with the Open Runtime for the app extent.
const APP_WINDOW: *mut u8 = 0x0d30_0000 as *mut u8;
const APP_WINDOW_SECTORS: usize = 128;

/// Read or write `count` sectors of the installed apps' extent at `first`
/// through the shared window. A count of 0 asks the size (read) or flushes (write).
fn apps(d: &mut Disk, write: bool, first: usize, count: usize) -> Message {
    if count == 0 {
        return if !write {
            Message::result(d.app_sectors() as u32)
        } else if d.flush().is_ok() {
            Message::result(0)
        } else {
            Message::error(OFFLINE)
        };
    }
    if count > APP_WINDOW_SECTORS || first.checked_add(count).map_or(true, |end| end > d.app_sectors()) {
        return Message::error(DENIED);
    }
    for k in 0..count {
        let at = unsafe { APP_WINDOW.add(k * 512) };
        let ok = if write {
            let mut s = [0u8; 512];
            unsafe { core::ptr::copy_nonoverlapping(at, s.as_mut_ptr(), 512) };
            d.app_write(first + k, &mut s).is_ok()
        } else {
            match d.app_read(first + k) {
                Ok(s) => {
                    unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), at, 512) };
                    true
                }
                Err(()) => false,
            }
        };
        if !ok {
            return Message::error(OFFLINE);
        }
    }
    Message::result(count as u32)
}

#[no_mangle]
pub extern "C" fn storage_task() {
    unsafe { clock_init() };
    let opened = Disk::open_checked();
    let mut diagnostic = opened.as_ref().err().copied().unwrap_or(b"");
    let mut disk = opened.ok();
    // The installed apps move to Android's userdata partition once VioletOS has claimed it.
    // Only a failed move is reported (the Open Runtime logs the room the apps have).
    if let Some(device) = disk.as_mut() {
        let adopted = device.adopt_userdata();
        if adopted.starts_with(b"APPS MOVE") || adopted == b"USERDATA READ" {
            diagnostic = adopted;
        }
    }
    let mut records = [None; 7];
    if let Some(device) = disk.as_mut() {
        #[cfg(boot_trace)]
        let _ = device.boot_record(6);
        for (index, record) in records.iter_mut().enumerate() {
            if index * 2 + 1 >= device.sectors {
                break;
            }
            let pair = device
                .read(index * 2)
                .and_then(|a| device.read(index * 2 + 1).map(|b| (a, b)));
            match pair {
                Ok((a, b)) => *record = recover(&a, &b).ok(),
                Err(()) => {
                    disk = None;
                    break;
                }
            }
        }
    }
    loop {
        let (badge, m) = Message::receive(2);
        #[cfg(package_catalog)]
        if m.op == 0x56464c53 && badge == 0 {
            let flushed = disk.as_mut().is_some_and(|device| device.flush().is_ok());
            (if flushed { Message::result(0) } else { Message::error(OFFLINE) }).reply();
            continue;
        }
        #[cfg(package_catalog)]
        if m.op == 0x56434d54 && badge == 0 {
            let bank = catalog::commit(m.value, m.arg);
            (if bank != 0 { Message::result(bank) } else { Message::error(OFFLINE) }).reply();
            continue;
        }
        if m.op == STORAGE_DIAGNOSTIC && badge == 3 {
            let mut r = Message::result(0);
            r.put_text(diagnostic);
            r.reply();
            continue;
        }
        if (STORAGE_APPS..STORAGE_APPS + 2).contains(&m.op) {
            let response = if badge != 3 {
                Message::error(DENIED)
            } else if let Some(d) = disk.as_mut() {
                apps(d, m.op == STORAGE_APPS + 1, m.value as usize, m.arg as usize)
            } else {
                Message::error(OFFLINE)
            };
            response.reply();
            continue;
        }
        if (STORAGE_FILE..STORAGE_FILE + 8).contains(&m.op) {
            let response = if badge != 3 {
                Message::error(DENIED)
            } else if let Some(d) = disk.as_mut() {
                files::dispatch(d, m.op - STORAGE_FILE, m).unwrap_or_else(Message::error)
            } else {
                Message::error(OFFLINE)
            };
            response.reply();
            continue;
        }
        let verb = m.op;
        let value = m.key;
        let index = (verb / 2) as usize;
        let mut result = None;
        if let (Some(device), Some(Some((slot, current)))) = (disk.as_mut(), records.get_mut(index))
        {
            if verb % 2 == 0 {
                result = Some(current.tenths);
            } else if let Some(generation) = current.generation.checked_add(1) {
                let next = Record {
                    generation,
                    tenths: value,
                };
                let target = 1 - *slot;
                if device
                    .commit(index * 2 + target, &mut next.encode())
                    .is_ok()
                {
                    *slot = target;
                    *current = next;
                    result = Some(value);
                } else {
                    // A timed-out device may still own the DMA buffers.
                    disk = None;
                }
            }
        }
        unsafe {
            storage_reply(result.is_some(), result.unwrap_or(0));
        }
    }
}
