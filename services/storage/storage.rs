//! One private durable value per installed app. Sectors 0 and 1 retain the
//! original stopwatch journal layout. The launcher owns sectors 2 and 3.
use crate::{
    block::Disk,
    journal::{recover, Record},
};
extern "C" {
    fn storage_receive(value: &mut u32) -> u32;
    fn storage_reply(ok: bool, value: u32);
    fn clock_init();
}

#[no_mangle]
pub extern "C" fn storage_task() {
    unsafe { clock_init() };
    let mut disk = Disk::open();
    let mut records = [None; 2];
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
        let mut value = 0;
        let verb = unsafe { storage_receive(&mut value) };
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
