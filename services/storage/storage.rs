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

#[no_mangle]
pub extern "C" fn storage_task() {
    unsafe { clock_init() };
    let opened = Disk::open_checked();
    let diagnostic = opened.as_ref().err().copied().unwrap_or(b"");
    let mut disk = opened.ok();
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
        if (STORAGE_FILE..STORAGE_FILE + 7).contains(&m.op) {
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
