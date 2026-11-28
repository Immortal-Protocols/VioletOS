//! Private files and a durable checkpoint per application. Sectors 0 and 1 retain the
//! original stopwatch journal layout. Launcher uses sectors 2–3 and Wi-Fi Setup uses 4–5.
#[path = "files.rs"]
mod files;
// /data: VioletFS on the data volume, served by path (vfs.rs).
#[path = "legacy.rs"]
mod legacy;
#[path = "vfs.rs"]
mod vfs;
#[path = "vfsproto.rs"]
mod vfsproto;
#[path = "violetfs/mod.rs"]
mod violetfs;
#[path = "volume.rs"]
mod volume;
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

// Full-system updates into the inactive boot slot (slotwrite.rs). Built for
// QEMU (no package_catalog) and, only with --cfg slot_ota, for the handset,
// which no build enables yet (targets/moto-g-pure/emmc.rs says why).
#[cfg(any(slot_ota, not(package_catalog)))]
#[path = "slotwrite.rs"]
mod slotwrite;
#[cfg(any(slot_ota, not(package_catalog)))]
mod ota {
    use super::slotwrite::{Error, Geometry, Medium, Sector, Updater, HASH_STEP};
    use super::{APP_WINDOW, APP_WINDOW_SECTORS};
    use crate::block::Disk;
    use crate::protocol::*;
    /// In APPSTORE's `value`: the inactive boot slot instead of the app extent.
    pub const SLOT: u32 = 1 << 31;
    /// With SLOT on EXECUTE: hash `arg` sectors (at most HASH_STEP) from `value`.
    pub const HASH: u32 = 1 << 30;
    /// The supervisor's requests (badge 0).
    pub const INFO: u32 = 0x4f54_4149; // "OTAI"
    pub const MANIFEST: u32 = 0x4f54_414d; // "OTAM"
    pub const DIGEST: u32 = 0x4f54_4144; // "OTAD"
    pub const SWITCH: u32 = 0x4f54_4153; // "OTAS"
    pub const MARK: u32 = 0x4f54_414b; // "OTAK"
    pub struct Raw<'a>(pub &'a mut Disk);
    impl Medium for Raw<'_> {
        fn read(&mut self, lba: u32) -> Result<Sector, ()> {
            self.0.slot_read(lba)
        }
        fn write(&mut self, lba: u32, s: &mut Sector) -> Result<(), ()> {
            self.0.slot_write(lba, s)
        }
        fn flush(&mut self) -> Result<(), ()> {
            self.0.flush()
        }
    }
    pub fn open(d: Option<&mut Disk>) -> Updater {
        let Some(d) = d else { return Updater::new(None, None) };
        let geo = d.slot_geometry().map(|(control, first, sectors)| Geometry { control, first, sectors });
        // QEMU: tools/ota_boot.py records the slot it started in slot_suffix.
        // The handset's bootloader passes it on the command line, which the
        // loader does not read yet: no running slot, so nothing is allowed.
        #[cfg(not(package_catalog))]
        return Updater::from_control(geo, &mut Raw(d));
        #[cfg(package_catalog)]
        return Updater::new(geo, None);
    }
    fn answer(r: Result<u32, Error>) -> Message {
        match r {
            Ok(v) => Message::result(v),
            Err(Error::Bounds | Error::Order | Error::Trial) => Message::error(DENIED),
            Err(Error::Digest | Error::Metadata(_)) => Message::error(INVALID),
            Err(Error::Unavailable | Error::Io) => Message::error(OFFLINE),
        }
    }
    /// The Open Runtime's requests (badge 3), through the app window.
    pub fn serve(d: &mut Disk, u: &mut Updater, write: bool, value: u32, count: u32) -> Message {
        let mut m = Raw(d);
        if !write && value == 0 && count == 0 {
            // Inactive slot size, the running slot in the top bit and, in
            // bit 30, that the running slot is still a trial (no update then).
            let cap = u.capacity();
            let trial = matches!(u.proven(&mut m), Ok(false)) as u32;
            return Message::result(if cap == 0 { 0 } else { cap | ((u.running().unwrap_or(0) as u32) << 31) | trial << 30 });
        }
        if write && value & HASH != 0 {
            if count == 0 || count > HASH_STEP {
                return Message::error(DENIED);
            }
            return answer(u.hash(&mut m, value & !HASH, count));
        }
        if write && count == 0 {
            return answer(m.flush().map(|_| 0).map_err(|_| Error::Io));
        }
        if count == 0 || count as usize > APP_WINDOW_SECTORS {
            return Message::error(DENIED);
        }
        let mut sectors = [[0u8; 512]; 8];
        let mut done = 0;
        while done < count {
            let n = (count - done).min(8);
            let window = unsafe { APP_WINDOW.add(done as usize * 512) };
            let batch = &mut sectors[..n as usize];
            let r = if write {
                for (k, s) in batch.iter_mut().enumerate() {
                    unsafe { core::ptr::copy_nonoverlapping(window.add(k * 512), s.as_mut_ptr(), 512) };
                }
                u.write(&mut m, value + done, batch)
            } else {
                u.read(&mut m, value + done, batch).map(|_| {
                    for (k, s) in batch.iter().enumerate() {
                        unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), window.add(k * 512), 512) };
                    }
                })
            };
            if let Err(e) = r {
                return answer(Err(e));
            }
            done += n;
        }
        Message::result(count)
    }
    /// The supervisor's requests (badge 0): read the manifest, the read-back
    /// digest, switch, and mark the running slot.
    pub fn supervisor(d: &mut Disk, u: &mut Updater, m: &Message) -> Option<Message> {
        let mut raw = Raw(d);
        let mut r = match m.op {
            INFO => Message::result(u.capacity()),
            MANIFEST => match u.manifest(&mut raw) {
                Ok(s) if m.value as usize <= 512 - 40 => {
                    let at = m.value as usize;
                    let mut r = Message::result(0);
                    r.data.copy_from_slice(&s[at..at + 32]);
                    r.tail.copy_from_slice(&s[at + 32..at + 40]);
                    r
                }
                Ok(_) => Message::error(DENIED),
                Err(e) => answer(Err(e)),
            },
            DIGEST => match u.digest(m.value) {
                Ok(digest) => {
                    let mut r = Message::result(m.value);
                    r.data = digest;
                    r
                }
                Err(e) => answer(Err(e)),
            },
            SWITCH => answer(u.switch(&mut raw, m.value, &m.data).map(|_| 0)),
            MARK => answer(u.mark_successful(&mut raw).map(u32::from)),
            _ => return None,
        };
        if r.op == OK {
            r.arg = u.running().unwrap_or(0) as u32;
        }
        Some(r)
    }
}

/// The 64 KiB window shared with the Open Runtime for the app extent.
const APP_WINDOW: *mut u8 = 0x0d30_0000 as *mut u8;
const APP_WINDOW_SECTORS: usize = 128;

/// /data: the filesystem server (vfs.rs) on the volume mount.rs places.
/// The server is a static that outlives every borrow of the disk, so the
/// disk is lent to it for the length of one call.
#[path = "mount.rs"]
mod mount;
static mut MOUNT: mount::Mount = mount::Mount::new();
fn data() -> &'static mut mount::Mount {
    unsafe { &mut *core::ptr::addr_of_mut!(MOUNT) }
}

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
    // /data's volume. A disk that is there always answers for it: a volume
    // that does not open (or has no room, or is not where it was made)
    // answers ENODEV and is reported, never OFFLINE ("nothing kept").
    // A disk lost after this (a failed read below or a timed-out commit)
    // does not turn into "no disk, nothing kept" for /data (see below).
    let had_disk = disk.is_some();
    if let Some(device) = disk.as_mut() {
        let wrong = data().open(device);
        if !wrong.is_empty() && diagnostic.is_empty() {
            diagnostic = wrong;
        }
    }
    let mut stages = [files::Stage::new(); 8];
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
    #[cfg(any(slot_ota, not(package_catalog)))]
    let mut updater = ota::open(disk.as_mut());
    loop {
        let (badge, m) = Message::receive(2);
        #[cfg(any(slot_ota, not(package_catalog)))]
        if badge == 0 {
            if let Some(r) = disk.as_mut().and_then(|d| ota::supervisor(d, &mut updater, &m)) {
                r.reply();
                continue;
            }
        }
        #[cfg(any(slot_ota, not(package_catalog)))]
        if (STORAGE_APPS..STORAGE_APPS + 2).contains(&m.op) && m.value & ota::SLOT != 0 {
            let response = if badge != 3 {
                Message::error(DENIED)
            } else if let Some(d) = disk.as_mut() {
                ota::serve(d, &mut updater, m.op == STORAGE_APPS + 1, m.value & !ota::SLOT, m.arg)
            } else {
                Message::error(OFFLINE)
            };
            response.reply();
            continue;
        }
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
        // /data, by path (vfsproto.rs): the Open Runtime's requests only.
        if (STORAGE_APPS..STORAGE_APPS + 2).contains(&m.op) && m.value & vfsproto::VFS != 0 {
            let response = match disk.as_mut() {
                _ if badge != 3 || m.op != STORAGE_APPS + 1 || m.value != vfsproto::VFS => Message::error(DENIED),
                Some(d) => {
                    let window = unsafe { core::slice::from_raw_parts_mut(APP_WINDOW, vfsproto::WINDOW) };
                    data().serve(d, window, m.arg as usize);
                    Message::result(0)
                }
                // OFFLINE says there never was a disk (nothing was kept); a
                // disk that was there and is lost now is a failure (EIO).
                None if had_disk => Message::error(INVALID),
                None => Message::error(OFFLINE),
            };
            response.reply();
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
                files::dispatch(d, &mut stages, m.op - STORAGE_FILE, m).unwrap_or_else(Message::error)
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
