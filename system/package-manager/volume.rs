//! Boot-accessible package catalogs on the dedicated QEMU virtio volume.
//! Each bank has a transaction descriptor followed by up to one MiB of data.
//! The prior bank stays readable until the new bytes and descriptor are durable.
#![no_std]
#[path = "../../services/storage/block.rs"]
mod block;
mod journal {
    pub type Sector = [u8; 512];
}
use block::Disk;
const BANK: usize = 2049;
const LIMIT: usize = 1024 * 1024;
extern "C" {
    fn crc32(bytes: *const u8, n: u32) -> u32;
}
fn sum(b: &[u8]) -> u32 {
    unsafe { crc32(b.as_ptr(), b.len() as u32) }
}
fn word(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}
#[derive(Clone, Copy)]
struct Entry {
    generation: u32,
    length: usize,
    crc: u32,
}
fn entries(d: &mut Disk) -> Result<[Option<Entry>; 2], ()> {
    let mut out = [None; 2];
    for (i, item) in out.iter_mut().enumerate() {
        let b = d.read(i * BANK)?;
        let length = word(&b, 8) as usize;
        if &b[..4] == b"VOL1" && (16..=LIMIT).contains(&length) && word(&b, 16) == sum(&b[..16]) {
            *item = Some(Entry {
                generation: word(&b, 4),
                length,
                crc: word(&b, 12),
            });
        }
    }
    Ok(out)
}
fn newest(records: &[Option<Entry>; 2]) -> usize {
    (records[1].map(|r| r.generation) > records[0].map(|r| r.generation)) as usize
}
#[no_mangle]
unsafe extern "C" fn volume_load(ptr: *mut u8) -> u32 {
    let Some(mut d) = Disk::open() else { return 0 };
    let Ok(records) = entries(&mut d) else {
        return 0;
    };
    let first = newest(&records);
    for index in [first, 1 - first] {
        let Some(record) = records[index] else {
            continue;
        };
        let dest = core::slice::from_raw_parts_mut(ptr, record.length);
        let mut good = true;
        for (n, chunk) in dest.chunks_mut(512).enumerate() {
            let Ok(sector) = d.read(index * BANK + 1 + n) else {
                good = false;
                break;
            };
            chunk.copy_from_slice(&sector[..chunk.len()]);
        }
        if good && sum(dest) == record.crc {
            return index as u32 + 1;
        }
    }
    0
}
#[no_mangle]
unsafe extern "C" fn volume_commit(ptr: *const u8, length: usize, active: u32) -> bool {
    if !(16..=LIMIT).contains(&length) {
        return false;
    }
    let Some(mut d) = Disk::open() else {
        return false;
    };
    if d.sectors < 2 * BANK {
        return false;
    }
    let Ok(records) = entries(&mut d) else {
        return false;
    };
    let first = newest(&records);
    let Some(generation) = records[first].map_or(0, |r| r.generation).checked_add(1) else {
        return false;
    };
    let index = if active == 1 {
        1
    } else if active == 2 {
        0
    } else {
        1 - first
    };
    let data = core::slice::from_raw_parts(ptr, length);
    for (n, chunk) in data.chunks(512).enumerate() {
        let mut sector = [0; 512];
        sector[..chunk.len()].copy_from_slice(chunk);
        if d.write(index * BANK + 1 + n, &mut sector).is_err() {
            return false;
        }
    }
    if d.flush().is_err() {
        return false;
    }
    let mut h = [0; 512];
    h[..4].copy_from_slice(b"VOL1");
    for (i, v) in [generation, length as u32, sum(data)]
        .into_iter()
        .enumerate()
    {
        h[4 * (i + 1)..4 * (i + 2)].copy_from_slice(&v.to_le_bytes());
    }
    let signature = sum(&h[..16]);
    h[16..20].copy_from_slice(&signature.to_le_bytes());
    d.commit(index * BANK, &mut h).is_ok()
}
#[no_mangle]
extern "C" fn volume_reject(bank: u32) -> bool {
    (1..=2).contains(&bank)
        && Disk::open()
            .is_some_and(|mut d| d.commit((bank as usize - 1) * BANK, &mut [0; 512]).is_ok())
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop()
    }
}
