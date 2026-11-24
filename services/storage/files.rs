//! App-private flat directories. Copy data to the inactive bank, flush, then
//! publish its directory entry. A reset always leaves a complete older file.
use crate::{
    block::Disk,
    journal::{crc32, Sector},
    protocol::*,
};
const SLOTS: usize = 4;
const PAGES: usize = 8;
const MAX: usize = PAGES * 512;
#[derive(Clone, Copy, Default)]
struct Entry {
    generation: u32,
    len: usize,
    name: [u8; 16],
    sums: [u32; PAGES],
}
fn u32at(bytes: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(bytes[n..n + 4].try_into().unwrap())
}
fn base(owner: u32, slot: usize, bank: usize) -> usize {
    32 + ((owner as usize * SLOTS + slot) * 2 + bank) * (PAGES + 1)
}
fn read_entry(d: &mut Disk, sector: usize) -> Result<Option<Entry>, u32> {
    let b = d.read(sector).map_err(|_| OFFLINE)?;
    if &b[..4] != b"VFS1" || u32at(&b, 508) != crc32(&b[..508]) {
        return Ok(None);
    }
    let mut e = Entry {
        generation: u32at(&b, 4),
        len: u32at(&b, 8) as usize,
        ..Entry::default()
    };
    if e.len > MAX {
        return Ok(None);
    }
    e.name.copy_from_slice(&b[12..28]);
    for (i, sum) in e.sums.iter_mut().enumerate() {
        *sum = u32at(&b, 28 + i * 4);
    }
    Ok(Some(e))
}
fn publish(d: &mut Disk, sector: usize, e: Entry) -> Result<(), u32> {
    let mut h = [0; 512];
    h[..4].copy_from_slice(b"VFS1");
    h[4..8].copy_from_slice(&e.generation.to_le_bytes());
    h[8..12].copy_from_slice(&(e.len as u32).to_le_bytes());
    h[12..28].copy_from_slice(&e.name);
    for (i, v) in e.sums.iter().enumerate() {
        h[28 + 4 * i..32 + 4 * i].copy_from_slice(&v.to_le_bytes());
    }
    let sum = crc32(&h[..508]);
    h[508..].copy_from_slice(&sum.to_le_bytes());
    d.commit(sector, &mut h).map_err(|_| OFFLINE)
}
fn current(d: &mut Disk, offset: usize) -> Result<(usize, Entry), u32> {
    let a = read_entry(d, offset)?.unwrap_or_default();
    let b = read_entry(d, offset + PAGES + 1)?.unwrap_or_default();
    Ok(if b.generation > a.generation {
        (1, b)
    } else {
        (0, a)
    })
}
fn page(d: &mut Disk, offset: usize, e: Entry, i: usize) -> Result<Sector, u32> {
    if i * 512 >= e.len {
        return Ok([0; 512]);
    }
    let b = d.read(offset + 1 + i).map_err(|_| OFFLINE)?;
    if crc32(&b) != e.sums[i] {
        return Err(OFFLINE);
    }
    Ok(b)
}
pub fn dispatch(d: &mut Disk, owner: u32, m: Message) -> Result<Message, u32> {
    if owner >= 8 {
        return Err(DENIED);
    }
    let bytes = m.text();
    let name: [u8; 16] = bytes[..16].try_into().unwrap();
    let end = name.iter().position(|b| *b == 0).unwrap_or(16);
    if m.key != FILE_LIST
        && (end == 0
            || name[end..].iter().any(|b| *b != 0)
            || name[..end]
                .iter()
                .any(|b| !b.is_ascii_alphanumeric() && !b"._-".contains(b)))
    {
        return Err(INVALID);
    }
    let mut empty = None;
    let mut found = None;
    for slot in 0..SLOTS {
        let loc = base(owner, slot, 0);
        let (bank, e) = current(d, loc)?;
        if m.key == FILE_LIST && slot == m.value as usize {
            let mut r = Message::result(e.len as u32);
            r.put_text(&e.name);
            return Ok(r);
        }
        if e.name == name && end > 0 {
            found = Some((loc, bank, e));
            break;
        }
        if e.name[0] == 0 && empty.is_none() {
            empty = Some((loc, bank, e));
        }
    }
    if m.key == FILE_LIST {
        return Err(INVALID);
    }
    let (loc, bank, mut e) = found
        .or_else(|| if m.key == FILE_WRITE { empty } else { None })
        .ok_or(EMPTY)?;
    let pos = m.value as usize;
    let n = m.arg as usize;
    let src = loc + bank * (PAGES + 1);
    let dst = loc + (1 - bank) * (PAGES + 1);
    if m.key == FILE_READ {
        if pos > e.len {
            return Err(INVALID);
        }
        let n = (e.len - pos).min(40);
        let mut r = Message::result(n as u32);
        r.arg = e.len as u32;
        let mut buf = [0; 40];
        let mut read = 0;
        while read < n {
            let idx = pos + read;
            let b = page(d, src, e, idx / 512)?;
            let take = (512 - idx % 512).min(n - read);
            buf[read..read + take].copy_from_slice(&b[idx % 512..idx % 512 + take]);
            read += take;
        }
        r.put_text(&buf);
        return Ok(r);
    }
    e.generation = e.generation.checked_add(1).ok_or(INVALID)?;
    if m.key == FILE_DELETE {
        e.name = [0; 16];
        e.len = 0;
        publish(d, dst, e)?;
        return Ok(Message::result(0));
    }
    let len = match m.key {
        FILE_TRUNCATE if pos <= e.len => pos,
        FILE_WRITE if pos <= e.len && n <= 24 && pos + n <= MAX => (pos + n).max(e.len),
        _ => return Err(INVALID),
    };
    for i in 0..(len + 511) / 512 {
        let mut b = page(d, src, e, i)?;
        if m.key == FILE_WRITE {
            let lo = pos.max(i * 512);
            let hi = (pos + n).min((i + 1) * 512);
            if hi > lo {
                b[lo - i * 512..hi - i * 512].copy_from_slice(&bytes[16 + lo - pos..16 + hi - pos]);
            }
        }
        if len < (i + 1) * 512 {
            b[len % 512..].fill(0);
        }
        e.sums[i] = crc32(&b);
        d.write(dst + 1 + i, &mut b).map_err(|_| OFFLINE)?;
    }
    d.flush().map_err(|_| OFFLINE)?;
    e.len = len;
    e.name = name;
    publish(d, dst, e)?;
    Ok(Message::result(len as u32))
}
