//! Private files and a durable checkpoint per application. Sectors 0 and 1 retain the
//! original stopwatch journal layout. Launcher uses sectors 2–3 and Wi-Fi Setup uses 4–5.
#[path = "files.rs"]
mod files;
use crate::protocol::*;
use crate::{
    block::Disk,
    journal::{recover, Record},
};
extern "C" {
    fn storage_reply(ok: bool, value: u32);
    fn clock_init();
}

#[no_mangle]
pub extern "C" fn storage_task() {
    unsafe { clock_init() };
    let mut disk = Disk::open();
    let mut records = [None; 7];
    if let Some(device) = disk.as_mut() {
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
