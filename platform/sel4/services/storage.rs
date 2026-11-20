//! IPC verbs LOAD and SAVE expose only the stopwatch checkpoint.
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
    unsafe {
        clock_init();
    }
    let mut volume = Disk::open().and_then(|mut disk| {
        let a = disk.read(0).ok()?;
        let b = disk.read(1).ok()?;
        let (slot, record) = recover(&a, &b).ok()?;
        Some((disk, slot, record))
    });
    loop {
        let mut value = 0;
        let verb = unsafe { storage_receive(&mut value) };
        let mut result = None;
        if let Some((disk, slot, current)) = volume.as_mut() {
            if verb == 0 {
                result = Some(current.tenths);
            }
            if verb == 1 {
                if let Some(generation) = current.generation.checked_add(1) {
                    let next = Record {
                        generation,
                        tenths: value,
                    };
                    if disk.commit(1 - *slot, &mut next.encode()).is_ok() {
                        *slot = 1 - *slot;
                        *current = next;
                        result = Some(value);
                    } else {
                        // A timed-out device may still own the buffers. Retire the queue.
                        volume = None;
                    }
                }
            }
        }
        unsafe {
            storage_reply(result.is_some(), result.unwrap_or(0));
        }
    }
}
